use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::Utc;

use crate::data::InstanceConfig;
use crate::instance_data::{
    load_load_order, load_manifest, manifest_path, manifests_differ, resolve_package_data_dir,
    resolve_synced_load_order, validate_manifest_against_disk, write_instance_launch_cfg,
    ManifestValidationResult, NerevarManifest, ResolvedOpenMwConfig,
};
use crate::reporter::{emit_event, EventSink};
use crate::runtime::{runtime_mismatch, RuntimeMismatch, TrustedRuntimeRepos};

use super::apply::apply_manifest_to_load_order;
use super::coordinator::SyncCoordinator;
use super::download::{download_manifest_files, persist_manifest, DownloadOutcome};
use super::fetch::{fetch_full_manifest, fetch_manifest_summary};
use super::metadata::{apply_manifest_metadata, write_synced_client_connection};
use super::progress::emit_sync_progress;
use super::sync_state::{instance_sync_status, load_sync_state, sync_is_complete};
use super::types::{InstanceSyncStatus, SyncOutcome, SyncPhase, SyncProgressEvent};

pub async fn run_instance_sync(
    sink: Arc<dyn EventSink>,
    coordinator: Arc<SyncCoordinator>,
    instance: &InstanceConfig,
    trusted: &TrustedRuntimeRepos,
) -> Result<SyncOutcome, String> {
    sync_if_needed(sink, coordinator, instance, false, trusted).await
}

/// When `force` is true, re-downloads every file. Otherwise resumes using sync-state checksums.
///
/// `trusted` is the player's trusted-repository list, which the version-lock
/// check reads: a host may pin the TES3MP tag its players need, so every sync
/// compares that tag against the instance's installed runtime and reports the
/// difference — as a `runtime-mismatch` event and in the returned
/// [`SyncOutcome`] — without ever downloading anything on the host's say-so.
pub async fn sync_if_needed(
    sink: Arc<dyn EventSink>,
    coordinator: Arc<SyncCoordinator>,
    instance: &InstanceConfig,
    force: bool,
    trusted: &TrustedRuntimeRepos,
) -> Result<SyncOutcome, String> {
    let host = instance
        .remote_host
        .as_deref()
        .ok_or_else(|| "Instance has no remote host configured".to_string())?;
    let port = instance
        .remote_sync_port
        .ok_or_else(|| "Instance has no remote sync port configured".to_string())?;
    let sync_password = instance.sync_password.as_deref();

    let instance_id = instance.id.clone();
    let data_dir = resolve_package_data_dir(instance);
    let cancel = coordinator.begin(&instance_id)?;

    let result = sync_if_needed_inner(
        sink.clone(),
        instance,
        &instance_id,
        host,
        port,
        sync_password,
        &data_dir,
        cancel.clone(),
        force,
        trusted,
    )
    .await;

    if cancel.load(Ordering::Relaxed) {
        coordinator.finish(&instance_id);
        return result;
    }

    coordinator.finish(&instance_id);
    result
}

async fn sync_if_needed_inner(
    sink: Arc<dyn EventSink>,
    instance: &InstanceConfig,
    instance_id: &str,
    host: &str,
    port: u16,
    sync_password: Option<&str>,
    data_dir: &Path,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    force: bool,
    trusted: &TrustedRuntimeRepos,
) -> Result<SyncOutcome, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Sync cancelled".to_string());
    }

    emit_sync_progress(
        &*sink,
        instance_id,
        SyncPhase::CheckingUpdates,
        "Checking host manifest for updates",
        0,
        1,
        0,
        0,
        None,
    );

    // The version lock, before anything is downloaded: the summary is where
    // the host states which TES3MP build it wants its players on.
    let mismatch = check_runtime_version(
        &*sink,
        instance,
        instance_id,
        host,
        port,
        sync_password,
        trusted,
    )
    .await;

    let remote = fetch_full_manifest(host, port, sync_password).await?;

    // Read the local manifest once and pass it around: the deltas below, the
    // already-up-to-date early return, and `finalize_after_sync` all want the
    // same bytes, and it can be large.
    let local_manifest = if manifest_path(data_dir).exists() {
        Some(load_manifest(data_dir)?)
    } else {
        None
    };

    let needs_download = match (force, local_manifest.as_ref()) {
        (true, _) => true,
        (false, None) => true,
        (false, Some(local)) => {
            if manifests_differ(local, &remote) {
                true
            } else {
                let state = load_sync_state(data_dir, &remote)?;
                !sync_is_complete(&state, &remote)
            }
        }
    };

    if let (false, Some(local)) = (needs_download, local_manifest.as_ref()) {
        // `needs_download == false` implies a local manifest exists (see the
        // match above), so this arm is the only reachable "nothing to do" path.
        // Nothing was transferred and nothing needed to be: the terminal event
        // reports this sync's transfer, not the size of the manifest.
        emit_sync_progress(
            &*sink,
            instance_id,
            SyncPhase::Complete,
            "Already up to date",
            0,
            0,
            0,
            0,
            None,
        );
        finalize_after_sync(instance, data_dir, local)?;
        return Ok(SyncOutcome {
            validation: ManifestValidationResult {
                valid: true,
                issues: Vec::new(),
            },
            runtime_mismatch: mismatch,
        });
    }

    if !force {
        if let Ok(status) = instance_sync_status(data_dir, &remote) {
            if status.can_resume {
                emit_sync_progress(
                    &*sink,
                    instance_id,
                    SyncPhase::CheckingUpdates,
                    format!(
                        "Resuming previous sync ({}% complete)",
                        status.percent_complete
                    ),
                    status.bytes_verified,
                    status.bytes_total.max(1),
                    status.files_verified as u64,
                    status.files_total as u64,
                    None,
                );
            }
        }
    }

    emit_sync_progress(
        &*sink,
        instance_id,
        SyncPhase::FetchingManifest,
        "Fetching manifest from host",
        0,
        1,
        0,
        0,
        None,
    );

    persist_manifest(data_dir, &remote)?;

    let download_result = download_manifest_files(
        sink.clone(),
        instance_id,
        host,
        port,
        sync_password,
        data_dir,
        &remote,
        force,
        cancel.clone(),
    )
    .await?;

    let bytes_transferred = match download_result {
        DownloadOutcome::Complete {
            bytes_done,
            bytes_transferred,
        } => {
            emit_sync_progress(
                &*sink,
                instance_id,
                SyncPhase::ApplyingLoadOrder,
                "Removing deleted mods and files",
                bytes_done,
                remote.total_download_bytes.max(1),
                0,
                0,
                None,
            );
            bytes_transferred
        }
        DownloadOutcome::Cancelled {
            bytes_done,
            bytes_total,
        } => {
            let status =
                instance_sync_status(data_dir, &remote).unwrap_or_else(|_| InstanceSyncStatus {
                    has_manifest: true,
                    can_resume: true,
                    is_complete: false,
                    bytes_verified: bytes_done,
                    bytes_total,
                    files_verified: 0,
                    files_total: 0,
                    percent_complete: if bytes_total > 0 {
                        ((bytes_done * 100) / bytes_total).min(100) as u8
                    } else {
                        0
                    },
                });
            emit_event(
                &*sink,
                "sync-progress",
                &SyncProgressEvent {
                    instance_id: instance_id.to_string(),
                    phase: SyncPhase::Cancelled,
                    message: format!(
                        "Sync paused at {}% — run Sync again to resume",
                        status.percent_complete
                    ),
                    bytes_done,
                    bytes_total,
                    files_done: status.files_verified as u64,
                    files_total: status.files_total as u64,
                    overall_percent: status.percent_complete,
                    current_file: None,
                },
            );
            return Err("Sync cancelled".to_string());
        }
    };

    if cancel.load(Ordering::Relaxed) {
        return Err("Sync cancelled".to_string());
    }

    crate::instance_data::prune_local_against_manifest(data_dir, &remote)?;

    if cancel.load(Ordering::Relaxed) {
        return Err("Sync cancelled".to_string());
    }

    let post_download_state = load_sync_state(data_dir, &remote)?;
    let validation = if sync_is_complete(&post_download_state, &remote) {
        ManifestValidationResult {
            valid: true,
            issues: Vec::new(),
        }
    } else {
        emit_sync_progress(
            &*sink,
            instance_id,
            SyncPhase::Validating,
            "Verifying downloaded files",
            0,
            1,
            0,
            0,
            None,
        );
        validate_manifest_against_disk(data_dir, &remote)
    };

    if !validation.valid {
        let message = format!("Validation failed ({} issues)", validation.issues.len());
        emit_sync_progress(
            &*sink,
            instance_id,
            SyncPhase::Failed,
            message,
            0,
            1,
            0,
            0,
            None,
        );
        return Ok(SyncOutcome {
            validation,
            runtime_mismatch: mismatch,
        });
    }

    finalize_after_sync(instance, data_dir, &remote)?;
    // The terminal event answers "what did this sync move?", so it reports the
    // bytes transferred rather than the manifest total — a run that downloaded
    // one 362-byte file reports 362/362, not the whole modlist's size. A
    // `Complete` download outcome moved every byte it planned to, so the
    // transferred count is both the done and the total here.
    emit_sync_progress(
        &*sink,
        instance_id,
        SyncPhase::Complete,
        "Sync complete",
        bytes_transferred,
        bytes_transferred,
        0,
        0,
        None,
    );
    Ok(SyncOutcome {
        validation,
        runtime_mismatch: mismatch,
    })
}

/// Compares the TES3MP version the host asks for against the one this
/// instance has installed, emitting `runtime-mismatch` when they differ.
///
/// A host that cannot be asked (its summary endpoint failed while the
/// manifest still loads) pins nothing: the sync goes ahead and reports no
/// requirement, rather than refusing to run because a check could not be made.
async fn check_runtime_version(
    sink: &dyn EventSink,
    instance: &InstanceConfig,
    instance_id: &str,
    host: &str,
    port: u16,
    sync_password: Option<&str>,
    trusted: &TrustedRuntimeRepos,
) -> Option<RuntimeMismatch> {
    let summary = match fetch_manifest_summary(host, port, sync_password).await {
        Ok(summary) => summary,
        Err(err) => {
            log::warn!("Could not read {host}'s runtime requirement: {err}");
            return None;
        }
    };

    let mismatch = runtime_mismatch(
        instance_id,
        summary.runtime_hint.as_ref(),
        instance.runtime.as_ref(),
        trusted,
    )?;
    log::warn!("{}", mismatch.message);
    emit_event(sink, "runtime-mismatch", &mismatch);
    Some(mismatch)
}

fn finalize_after_sync(
    instance: &InstanceConfig,
    data_dir: &Path,
    manifest: &NerevarManifest,
) -> Result<(), String> {
    apply_manifest_to_load_order(data_dir, manifest)?;
    let load_order = load_load_order(data_dir)?;
    let resolved: ResolvedOpenMwConfig =
        resolve_synced_load_order(data_dir, &load_order, manifest)?;
    write_instance_launch_cfg(
        data_dir,
        &resolved,
        &manifest.instance_settings.openmw_cfg_overrides,
    )?;
    write_synced_client_connection(instance, manifest)?;
    crate::instance_settings::persist_settings_from_manifest(
        data_dir,
        &manifest.instance_settings,
    )?;
    Ok(())
}

pub fn touch_last_synced(instance: &mut InstanceConfig, manifest: &NerevarManifest) {
    instance.last_synced_at = Some(Utc::now().to_rfc3339());
    apply_manifest_metadata(instance, manifest);
}
