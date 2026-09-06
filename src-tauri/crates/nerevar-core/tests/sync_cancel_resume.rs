//! Cancellation and resume coverage for the client download engine, WITHOUT the Tauri layer.
//!
//! Companion to `sync_roundtrip.rs`, which drives the clean from-empty happy path through
//! the real embedded axum server and `sync_client/download.rs::download_manifest_files`.
//! This file covers the two paths that one explicitly leaves out:
//!
//!   1. mid-flight cancellation — the `cancel: AtomicBool` flipping while the 16-worker
//!      pool is in flight, and what that leaves on disk and in `sync-state.json`;
//!   2. resume-after-partial — a second `download_manifest_files` adopting what a prior
//!      run left behind and skipping it, including files present with no sync-state at
//!      all, and re-downloading files whose bytes are wrong.
//!
//! Like `sync_roundtrip.rs` this only touches `nerevar-core`'s public API, and
//! `CollectingEventSink` is reachable through the crate's self-dev-dep `test-util`
//! feature.
//!
//! Determinism: cancellation is NOT timed. `CancelOnFirstFile` is a real `EventSink` that
//! flips the cancel flag the moment the engine reports its first completed file in the
//! Downloading phase. The engine's progress throttler is primed to emit immediately, and
//! that emit happens synchronously inside the worker before it pops its next job, so the
//! flag is set while at least `FILE_COUNT - MAX_CONCURRENT_DOWNLOADS` jobs are still
//! unclaimed. Every worker re-checks the flag at the top of its loop and between response
//! chunks, so the run is guaranteed to end `Cancelled` and guaranteed to be partial.
//! Nothing here sleeps.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nerevar_core::data::InstanceConfig;
use nerevar_core::instance_data::{
    build_manifest, file_checksum, load_load_order, manifest_path, package_abs_path,
    scan_and_merge_load_order, ManifestFileEntry, ManifestPackage, NerevarManifest,
};
use nerevar_core::nerevar_server::state::ServerContext;
use nerevar_core::nerevar_server::{serve, try_bind};
use nerevar_core::reporter::{CollectingEventSink, EventSink};
use nerevar_core::sync_client::download::{download_manifest_files, DownloadOutcome};
use nerevar_core::sync_client::sync::sync_if_needed;
use nerevar_core::sync_client::sync_state::{is_verified_in, load_sync_state, sync_is_complete};
use nerevar_core::sync_client::SyncCoordinator;
use nerevar_core::sync_client::{fetch_full_manifest, fetch_manifest_summary};
use nerevar_core::sync_host::{new_shared_hosting_manifest_cache, new_shared_sync_host};

const SYNC_PASSWORD: &str = "swordfish";

/// Fixture shape. 3 packages x (16 payload files + 1 plugin) = 51 manifest files, ~12 MB.
///
/// The file COUNT is the load-bearing number: it must exceed the engine's
/// `MAX_CONCURRENT_DOWNLOADS` (16) by a wide margin so that cancellation always lands
/// with jobs still unclaimed. The file SIZE only makes the cancellation land mid-transfer
/// on localhost rather than between files; it is kept small enough that a full run of this
/// binary stays a couple of seconds.
const PACKAGES: usize = 3;
const PAYLOAD_FILES_PER_PACKAGE: usize = 16;
const PAYLOAD_FILE_BYTES: usize = 256 * 1024;

/// Upper bound on a single engine call. Not a synchronisation device — every test would
/// pass with an infinite bound; this only turns a hang into a failure.
const ENGINE_CALL_BUDGET: Duration = Duration::from_secs(60);

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// Deterministic, non-repeating bytes so every fixture file has a distinct checksum.
fn pseudo_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 8);
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// An `EventSink` that flips `cancel` as soon as the engine reports a completed file in
/// the Downloading phase, and records every event for later assertions.
struct CancelOnFirstFile {
    inner: Arc<CollectingEventSink>,
    cancel: Arc<AtomicBool>,
    tripped: AtomicBool,
}

impl CancelOnFirstFile {
    fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            inner: Arc::new(CollectingEventSink::default()),
            cancel,
            tripped: AtomicBool::new(false),
        }
    }
}

impl EventSink for CancelOnFirstFile {
    fn emit(&self, event: &'static str, payload: serde_json::Value) {
        let is_download_progress = event == "sync-progress"
            && payload.get("phase").and_then(|p| p.as_str()) == Some("downloading")
            && payload
                .get("filesDone")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                >= 1;
        self.inner.emit(event, payload);
        if is_download_progress {
            self.tripped.store(true, Ordering::SeqCst);
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
}

/// A live host: a real instance data dir, a real manifest, and the real axum server.
struct Harness {
    root: PathBuf,
    host_data_dir: PathBuf,
    client_dir: PathBuf,
    port: u16,
    manifest: NerevarManifest,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "nerevar-cancel-resume-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let instance_root = root.join("instance");
        let host_data_dir = instance_root.join("data");
        let client_dir = root.join("client");
        std::fs::create_dir_all(&host_data_dir).unwrap();
        std::fs::create_dir_all(&client_dir).unwrap();

        for pkg in 0..PACKAGES {
            let pkg_dir = host_data_dir.join(format!("Mod{pkg}"));
            write(
                &pkg_dir.join(format!("mod{pkg}.esp")),
                format!("Mod{pkg} plugin payload").as_bytes(),
            );
            for file in 0..PAYLOAD_FILES_PER_PACKAGE {
                let seed = (pkg as u64) << 32 | file as u64;
                write(
                    &pkg_dir.join(format!("meshes/asset{file:02}.nif")),
                    &pseudo_bytes(seed.wrapping_add(1), PAYLOAD_FILE_BYTES),
                );
            }
        }

        // Minimal TES3MP layout that `build_manifest` reads/writes through (same shape as
        // `sync_roundtrip.rs`).
        let tes3mp_dir = instance_root.join("tes3mp");
        write(
            &tes3mp_dir.join("tes3mp-server-default.cfg"),
            b"[General]\nhostname = Cancel Resume Test\nport = 25565\npassword = \n",
        );
        std::fs::create_dir_all(tes3mp_dir.join("server/data")).unwrap();
        write(
            &tes3mp_dir.join("server/scripts/config.lua"),
            b"config = {}\n",
        );

        let mut no_progress = None;
        scan_and_merge_load_order(&host_data_dir, &mut no_progress).expect("scan + merge");
        let load_order = load_load_order(&host_data_dir).expect("load order");
        let manifest = build_manifest(
            "cancel-resume-instance",
            "Cancel Resume Instance",
            &instance_root,
            &host_data_dir,
            &load_order,
            &mut no_progress,
        )
        .expect("build manifest");
        assert!(manifest_path(&host_data_dir).is_file());

        let sync_host = new_shared_sync_host();
        {
            let mut host = sync_host.lock().unwrap();
            host.hosting_instance_id = Some("cancel-resume-instance".to_string());
            host.hosting_data_dir = Some(host_data_dir.clone());
            host.hosting_instance_root = Some(instance_root.clone());
            host.hosting_sync_password = Some(SYNC_PASSWORD.to_string());
        }
        let ctx = ServerContext::new(sync_host, new_shared_hosting_manifest_cache());

        let listener = try_bind(0).await.expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        let server = tokio::spawn(async move {
            let _ = serve(listener, ctx).await;
        });

        // Fetch the manifest over the wire: the client side must work from what the server
        // served, not from the in-process host copy.
        let fetched = fetch_full_manifest("127.0.0.1", port, Some(SYNC_PASSWORD))
            .await
            .expect("fetch manifest");
        assert_eq!(fetched.total_download_bytes, manifest.total_download_bytes);

        Self {
            root,
            host_data_dir,
            client_dir,
            port,
            manifest: fetched,
            server,
        }
    }

    fn file_count(&self) -> u64 {
        self.manifest
            .packages
            .iter()
            .map(|p| p.files.len() as u64)
            .sum()
    }

    fn client_dest(&self, package: &ManifestPackage, file: &ManifestFileEntry) -> PathBuf {
        package_abs_path(&self.client_dir, &package.relative_dir).join(&file.path)
    }

    fn host_source(&self, package: &ManifestPackage, file: &ManifestFileEntry) -> PathBuf {
        package_abs_path(&self.host_data_dir, &package.relative_dir).join(&file.path)
    }

    /// Copy every manifest file into the client layout, byte for byte, leaving no
    /// sync-state behind.
    fn seed_client_from_host(&self) {
        for package in &self.manifest.packages {
            for file in &package.files {
                let dest = self.client_dest(package, file);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::copy(self.host_source(package, file), &dest).unwrap();
            }
        }
    }

    /// Run the real engine against the live server.
    async fn run_engine(
        &self,
        sink: Arc<dyn EventSink>,
        cancel: Arc<AtomicBool>,
        force: bool,
    ) -> DownloadOutcome {
        self.run_engine_on_port(sink, cancel, force, self.port)
            .await
    }

    async fn run_engine_on_port(
        &self,
        sink: Arc<dyn EventSink>,
        cancel: Arc<AtomicBool>,
        force: bool,
        port: u16,
    ) -> DownloadOutcome {
        tokio::time::timeout(
            ENGINE_CALL_BUDGET,
            download_manifest_files(
                sink,
                "cancel-resume-instance",
                "127.0.0.1",
                port,
                Some(SYNC_PASSWORD),
                &self.client_dir,
                &self.manifest,
                force,
                cancel,
            ),
        )
        .await
        .expect("download_manifest_files must return within the budget")
        .expect("download_manifest_files must not error")
    }

    /// Every manifest file present on disk with the checksum the manifest was built with.
    fn assert_client_fully_correct(&self) {
        for package in &self.manifest.packages {
            for file in &package.files {
                let dest = self.client_dest(package, file);
                assert!(dest.is_file(), "missing {}", dest.display());
                assert_eq!(
                    file_checksum(&dest).expect("checksum"),
                    file.checksum,
                    "checksum mismatch for {}",
                    dest.display()
                );
            }
        }
    }

    fn teardown(self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Payloads of the `sync-progress` events emitted for a given phase, in order.
fn phase_events(sink: &CollectingEventSink, phase: &str) -> Vec<serde_json::Value> {
    sink.events()
        .into_iter()
        .filter(|(name, payload)| {
            *name == "sync-progress" && payload.get("phase").and_then(|p| p.as_str()) == Some(phase)
        })
        .map(|(_, payload)| payload)
        .collect()
}

fn as_u64(payload: &serde_json::Value, key: &str) -> u64 {
    payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("event payload missing numeric {key}: {payload}"))
}

fn message(payload: &serde_json::Value) -> String {
    payload
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Number of manifest files the on-disk sync-state currently claims are verified.
fn verified_count(harness: &Harness) -> u64 {
    let state = load_sync_state(&harness.client_dir, &harness.manifest).expect("sync state");
    let mut count = 0;
    for package in &harness.manifest.packages {
        for file in &package.files {
            if is_verified_in(&state.completed, &package.id, file) {
                count += 1;
            }
        }
    }
    count
}

/// Cancel the flag mid-flight and return the outcome plus the recorded events.
async fn cancelled_first_run(harness: &Harness) -> (u64, u64, Arc<CollectingEventSink>) {
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(CancelOnFirstFile::new(cancel.clone()));
    let collected = sink.inner.clone();

    let outcome = harness
        .run_engine(sink.clone(), cancel.clone(), false)
        .await;

    assert!(
        sink.tripped.load(Ordering::SeqCst),
        "the cancelling sink never saw a completed-file progress event"
    );
    match outcome {
        DownloadOutcome::Cancelled {
            bytes_done,
            bytes_total,
        } => (bytes_done, bytes_total, collected),
        DownloadOutcome::Complete { .. } => {
            panic!("engine reported Complete even though cancel was flipped mid-flight")
        }
    }
}

/// DECIDES: flipping `cancel` while the worker pool is mid-flight ends the run as
/// `DownloadOutcome::Cancelled` rather than `Complete` or an error; the run stops while
/// work genuinely remains; the server survives it; and everything the run left behind is
/// trustworthy — every entry in `sync-state.json` names a file that is on disk with the
/// manifest's checksum, no file at a manifest destination is torn or partial, and
/// `sync_is_complete` reports false so the next sync knows to resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_mid_flight_yields_cancelled_outcome() {
    let harness = Harness::start("cancel").await;
    let files_total = harness.file_count();

    let (bytes_done, bytes_total, collected) = cancelled_first_run(&harness).await;

    assert_eq!(
        bytes_total,
        harness.manifest.total_download_bytes.max(1),
        "Cancelled outcome reports the manifest's byte total"
    );
    assert!(
        bytes_done > 0 && bytes_done < bytes_total,
        "cancellation must land partway: {bytes_done} of {bytes_total} bytes"
    );

    // The engine reported progress before it stopped, and every event is a sync-progress.
    let events = collected.events();
    assert!(!events.is_empty());
    for (name, _) in &events {
        assert_eq!(*name, "sync-progress");
    }

    // The run stopped with work still outstanding.
    let verified = verified_count(&harness);
    assert!(
        verified >= 1,
        "at least one file completed before cancelling"
    );
    assert!(
        verified < files_total,
        "cancellation must leave files undone: {verified} of {files_total} verified"
    );

    // sync-state.json is on disk and is honest: every entry it marks verified names a file
    // that exists with the manifest checksum.
    let state_path = manifest_path(&harness.client_dir)
        .parent()
        .unwrap()
        .join("sync-state.json");
    assert!(
        state_path.is_file(),
        "a cancelled run must persist sync-state.json for the resume"
    );
    let state = load_sync_state(&harness.client_dir, &harness.manifest).expect("sync state");
    assert!(
        !sync_is_complete(&state, &harness.manifest),
        "a cancelled run is never complete"
    );
    for package in &harness.manifest.packages {
        for file in &package.files {
            let dest = harness.client_dest(package, file);
            if is_verified_in(&state.completed, &package.id, file) {
                assert!(
                    dest.is_file(),
                    "sync-state marks {} verified but it is not on disk",
                    dest.display()
                );
                assert_eq!(
                    file_checksum(&dest).expect("checksum"),
                    file.checksum,
                    "sync-state marks {} verified but its bytes are wrong",
                    dest.display()
                );
            }
            // Whether or not it is in sync-state, anything at a manifest destination must
            // be whole: the engine downloads into `.nerevar-part` and renames only after
            // verifying, so a cancel can never publish a torn file.
            if dest.is_file() {
                assert_eq!(
                    file_checksum(&dest).expect("checksum"),
                    file.checksum,
                    "cancelled run left a partial/corrupt file at {}",
                    dest.display()
                );
            }
        }
    }

    // The server is unaffected by a client-side cancellation.
    let summary = fetch_manifest_summary("127.0.0.1", harness.port, Some(SYNC_PASSWORD))
        .await
        .expect("server must still be serving after a cancelled download");
    assert_eq!(summary.package_count as usize, PACKAGES);

    harness.teardown();
}

/// DECIDES: a second `download_manifest_files` after a cancelled run resumes instead of
/// starting over — it adopts what the first run left on disk, reports the already-verified
/// files as done before the first byte of the second run moves, downloads only the
/// remainder, and finishes `Complete` with `sync_is_complete` true.
///
/// Note on the byte accounting: the engine seeds `bytes_done` with the bytes of every
/// already-verified file (`skipped_bytes`), so a resumed run's `Complete { bytes_done }`
/// equals the manifest total exactly as a from-empty run's does. `bytes_done` therefore
/// cannot distinguish a skip from a re-download. Two observables can: the first
/// Downloading-phase event, whose `filesDone`/`bytesDone` are the skip counts and whose
/// message is "Resuming download — N files remaining", and `Complete { bytes_transferred }`,
/// which counts only what this run pulled over the network — asserted below to be the
/// manifest total minus what the resume adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_adopts_completed_files_and_finishes() {
    let harness = Harness::start("resume").await;
    let files_total = harness.file_count();

    cancelled_first_run(&harness).await;
    let verified_after_cancel = verified_count(&harness);
    assert!(verified_after_cancel > 0 && verified_after_cancel < files_total);

    let sink = Arc::new(CollectingEventSink::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = harness.run_engine(sink.clone(), cancel, false).await;

    let (bytes_done, bytes_transferred) = match outcome {
        DownloadOutcome::Complete {
            bytes_done,
            bytes_transferred,
        } => (bytes_done, bytes_transferred),
        DownloadOutcome::Cancelled { .. } => panic!("resume run must not be cancelled"),
    };
    assert_eq!(
        bytes_done, harness.manifest.total_download_bytes,
        "a resumed Complete still accounts for every manifest byte (skips included)"
    );
    assert!(
        bytes_transferred > 0 && bytes_transferred < bytes_done,
        "a resumed run transfers only the remainder: {bytes_transferred} of {bytes_done}"
    );

    let downloading = phase_events(&sink, "downloading");
    assert!(
        !downloading.is_empty(),
        "resume must reach the Downloading phase"
    );

    // The first Downloading event is emitted before any worker starts; its counters are
    // purely what the resume adopted.
    let first = &downloading[0];
    let skipped_files = as_u64(first, "filesDone");
    assert!(
        skipped_files >= verified_after_cancel,
        "resume must adopt at least what the cancelled run recorded ({verified_after_cancel}), got {skipped_files}"
    );
    assert!(
        skipped_files > 0 && skipped_files < files_total,
        "resume must skip some files and still have work to do: {skipped_files} of {files_total}"
    );
    assert!(
        as_u64(first, "bytesDone") > 0,
        "resume starts with the adopted bytes already counted"
    );
    // The first Downloading event's `bytesDone` IS `skipped_bytes`, so this pins
    // `bytes_transferred` to the exact remainder rather than merely "less than the total".
    assert_eq!(
        bytes_done - bytes_transferred,
        as_u64(first, "bytesDone"),
        "bytes_transferred must be the manifest total minus the adopted bytes"
    );
    assert!(
        message(first).starts_with("Resuming download"),
        "expected a resuming message, got {:?}",
        message(first)
    );

    let last = downloading.last().unwrap();
    assert_eq!(message(last), "Download complete");
    assert_eq!(as_u64(last, "filesDone"), files_total);
    assert_eq!(
        as_u64(last, "bytesDone"),
        harness.manifest.total_download_bytes
    );

    harness.assert_client_fully_correct();
    let state = load_sync_state(&harness.client_dir, &harness.manifest).expect("sync state");
    assert!(sync_is_complete(&state, &harness.manifest));

    harness.teardown();
}

/// DECIDES: files that are already correct on disk but recorded nowhere — the case of a
/// sync-state file that was lost, or a data dir copied in from elsewhere — are adopted by
/// checksum rather than re-downloaded.
///
/// Proof that nothing is fetched is structural, not statistical: the engine is pointed at
/// a port with nothing listening on it. Any download attempt would be a connection error
/// and the call would return `Err`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_adopts_files_present_on_disk_without_state() {
    let harness = Harness::start("adopt").await;
    let files_total = harness.file_count();

    harness.seed_client_from_host();
    let state_path = manifest_path(&harness.client_dir)
        .parent()
        .map(|dir| dir.join("sync-state.json"));
    assert!(
        state_path.as_ref().is_none_or(|p| !p.exists()),
        "the fixture must start with no sync-state at all"
    );

    // A port nothing is listening on: bind it, learn the number, release it.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let sink = Arc::new(CollectingEventSink::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = harness
        .run_engine_on_port(sink.clone(), cancel, false, dead_port)
        .await;

    match outcome {
        DownloadOutcome::Complete { bytes_done, .. } => {
            assert_eq!(bytes_done, harness.manifest.total_download_bytes)
        }
        DownloadOutcome::Cancelled { .. } => panic!("adoption run must not be cancelled"),
    }

    assert!(
        !phase_events(&sink, "verifyingExisting").is_empty(),
        "adoption runs through the VerifyingExisting phase"
    );
    let downloading = phase_events(&sink, "downloading");
    assert_eq!(
        downloading.len(),
        1,
        "with everything adopted the engine emits exactly one Downloading event and spawns no workers"
    );
    assert_eq!(message(&downloading[0]), "All files already verified");
    assert_eq!(as_u64(&downloading[0], "filesDone"), files_total);

    harness.assert_client_fully_correct();
    let state = load_sync_state(&harness.client_dir, &harness.manifest).expect("sync state");
    assert!(sync_is_complete(&state, &harness.manifest));

    harness.teardown();
}

/// DECIDES: a file on disk whose bytes do not match the manifest is NOT adopted — neither
/// when it is the wrong size nor when it is the right size with wrong contents — and is
/// re-downloaded so the run ends with correct bytes. Everything else on disk is still
/// adopted, so only the bad files are fetched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_redownloads_corrupt_file() {
    let harness = Harness::start("corrupt").await;
    let files_total = harness.file_count();

    harness.seed_client_from_host();

    // Two flavours of wrong: same size / different bytes (only a checksum catches it), and
    // truncated (the cheap size check catches it).
    let package = &harness.manifest.packages[0];
    let same_size = &package.files[0];
    let truncated = &package.files[1];

    let same_size_path = harness.client_dest(package, same_size);
    std::fs::write(
        &same_size_path,
        pseudo_bytes(0xDEAD_BEEF, same_size.size as usize),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&same_size_path).unwrap().len(),
        same_size.size,
        "the same-size corruption must keep the size"
    );
    assert_ne!(file_checksum(&same_size_path).unwrap(), same_size.checksum);

    let truncated_path = harness.client_dest(package, truncated);
    std::fs::write(&truncated_path, b"short").unwrap();

    let sink = Arc::new(CollectingEventSink::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = harness.run_engine(sink.clone(), cancel, false).await;
    match outcome {
        DownloadOutcome::Complete { bytes_done, .. } => {
            assert_eq!(bytes_done, harness.manifest.total_download_bytes)
        }
        DownloadOutcome::Cancelled { .. } => panic!("repair run must not be cancelled"),
    }

    // Exactly the two bad files were queued; the other 49 were adopted.
    let downloading = phase_events(&sink, "downloading");
    let first = &downloading[0];
    assert_eq!(
        as_u64(first, "filesDone"),
        files_total - 2,
        "only the two corrupt files should have been queued"
    );
    assert!(
        message(first).contains("2 files remaining"),
        "expected 2 files remaining, got {:?}",
        message(first)
    );

    harness.assert_client_fully_correct();
    let state = load_sync_state(&harness.client_dir, &harness.manifest).expect("sync state");
    assert!(sync_is_complete(&state, &harness.manifest));

    harness.teardown();
}

/// DECIDES (and pins current, deliberate behaviour): `sync-state.json` is a hash CACHE,
/// not a verifier. A file that is marked verified and is then corrupted on disk behind the
/// engine's back is NOT re-checked by a normal resume — `is_verified_in` compares the
/// state's recorded checksum against the manifest's and never re-hashes the file — so the
/// corruption survives. `force = true` clears the state and repairs it.
///
/// This is the price of not re-hashing every file on every sync. The test exists so that
/// if the trade-off is ever revisited, the change is a visible test change rather than a
/// silent one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stale_verified_state_entry_is_trusted_until_a_forced_sync() {
    let harness = Harness::start("stale").await;

    // A clean, complete run, so sync-state records every file as verified.
    let outcome = harness
        .run_engine(
            Arc::new(CollectingEventSink::default()),
            Arc::new(AtomicBool::new(false)),
            false,
        )
        .await;
    assert!(matches!(outcome, DownloadOutcome::Complete { .. }));
    harness.assert_client_fully_correct();

    // Corrupt a file behind the engine's back, keeping its size.
    let package = &harness.manifest.packages[0];
    let file = &package.files[0];
    let path = harness.client_dest(package, file);
    std::fs::write(&path, pseudo_bytes(0xC0FF_EE00, file.size as usize)).unwrap();

    // A normal resume trusts the cached entry and leaves the corruption in place.
    let sink = Arc::new(CollectingEventSink::default());
    let outcome = harness
        .run_engine(sink.clone(), Arc::new(AtomicBool::new(false)), false)
        .await;
    assert!(matches!(outcome, DownloadOutcome::Complete { .. }));
    let downloading = phase_events(&sink, "downloading");
    assert_eq!(downloading.len(), 1);
    assert_eq!(message(&downloading[0]), "All files already verified");
    assert_ne!(
        file_checksum(&path).unwrap(),
        file.checksum,
        "documented behaviour: a cached verified entry is not re-hashed, so the corruption survives"
    );

    // `force` clears sync-state and re-downloads everything, repairing it.
    let sink = Arc::new(CollectingEventSink::default());
    let outcome = harness
        .run_engine(sink.clone(), Arc::new(AtomicBool::new(false)), true)
        .await;
    match outcome {
        DownloadOutcome::Complete { bytes_done, .. } => {
            assert_eq!(bytes_done, harness.manifest.total_download_bytes)
        }
        DownloadOutcome::Cancelled { .. } => panic!("forced run must not be cancelled"),
    }
    assert!(
        phase_events(&sink, "verifyingExisting").is_empty(),
        "a forced run skips the adopt/verify phase entirely"
    );
    harness.assert_client_fully_correct();

    harness.teardown();
}

/// DECIDES: the higher-level entry point the GUI and the daemon actually call —
/// `sync_client::sync::sync_if_needed` — short-circuits on a second call. Once a sync has
/// completed, the next one compares the persisted manifest against the served one, finds
/// sync-state complete, reports "Already up to date" and never enters the Downloading
/// phase.
///
/// Also DECIDES the terminal event's byte counters: `Complete` reports what the sync
/// transferred over what it needed to transfer, NOT the size of the manifest. A from-empty
/// run therefore reports the manifest total, and the short-circuiting second run reports
/// `0`/`0` — it moved nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sync_if_needed_reports_already_up_to_date_on_the_second_call() {
    let harness = Harness::start("sync-if-needed").await;

    // A client instance laid out the way `InstanceConfig` expects: `{path}/data` for
    // packages and `{path}/tes3mp/` holding the client cfg that finalisation patches.
    let instance_root = harness.root.join("client-instance");
    let instance_data = instance_root.join("data");
    std::fs::create_dir_all(&instance_data).unwrap();
    write(
        &instance_root.join("tes3mp/tes3mp-client-default.cfg"),
        b"[General]\ndestinationAddress = 0.0.0.0\nport = 0\npassword = \n",
    );

    let instance = InstanceConfig {
        id: "cancel-resume-instance".to_string(),
        name: "Cancel Resume Instance".to_string(),
        description: String::new(),
        path: instance_root.to_string_lossy().into_owned(),
        data_dir: instance_data.to_string_lossy().into_owned(),
        release_id: None,
        runtime: None,
        runtime_hint: None,
        remote_host: Some("127.0.0.1".to_string()),
        remote_sync_port: Some(harness.port),
        last_synced_at: None,
        tes3mp_server_port: None,
        sync_password: Some(SYNC_PASSWORD.to_string()),
    };
    let coordinator = Arc::new(SyncCoordinator::new());

    let first_sink = Arc::new(CollectingEventSink::default());
    let first = sync_if_needed(first_sink.clone(), coordinator.clone(), &instance, false)
        .await
        .expect("first sync");
    assert!(first.valid, "first sync issues: {:?}", first.issues);
    assert!(
        !phase_events(&first_sink, "downloading").is_empty(),
        "the first sync must actually download"
    );
    let first_complete = phase_events(&first_sink, "complete");
    assert_eq!(first_complete.len(), 1, "one terminal event per sync");
    assert_eq!(
        as_u64(&first_complete[0], "bytesDone"),
        harness.manifest.total_download_bytes,
        "a from-empty sync transferred every manifest byte"
    );
    assert_eq!(
        as_u64(&first_complete[0], "bytesTotal"),
        harness.manifest.total_download_bytes,
        "and needed to transfer exactly that many"
    );

    let second_sink = Arc::new(CollectingEventSink::default());
    let second = sync_if_needed(second_sink.clone(), coordinator, &instance, false)
        .await
        .expect("second sync");
    assert!(second.valid);
    assert!(
        phase_events(&second_sink, "downloading").is_empty(),
        "an up-to-date instance must not enter the Downloading phase"
    );
    let complete = phase_events(&second_sink, "complete");
    assert_eq!(
        complete.len(),
        1,
        "expected exactly one Complete event, got {complete:?}"
    );
    assert_eq!(message(&complete[0]), "Already up to date");
    assert_eq!(
        as_u64(&complete[0], "bytesDone"),
        0,
        "an up-to-date sync transferred nothing"
    );
    assert_eq!(
        as_u64(&complete[0], "bytesTotal"),
        0,
        "and needed to transfer nothing — not the manifest total"
    );
    assert_eq!(
        as_u64(&complete[0], "overallPercent"),
        100,
        "a Complete event is 100% however few bytes moved"
    );

    harness.teardown();
}
