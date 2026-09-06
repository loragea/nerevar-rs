use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::admin::ServerRestartState;
use crate::process_manager::ProcessManager;
use crate::reporter::{EventSink, NullEventSink};
use crate::sync_host::{SharedHostingManifestCache, SharedSyncHost};

/// What the embedded HTTP server can reach.
///
/// The two sync fields are all the player-facing routes need. The two below
/// them are what `/admin` adds, and both are optional in practice: an
/// embedder that supplies neither (the desktop app) still serves `/admin`
/// correctly, it just logs audit lines nowhere and reports the TES3MP
/// server's state as unknown.
#[derive(Clone)]
pub struct ServerContext {
    pub sync_host: SharedSyncHost,
    pub manifest_cache: SharedHostingManifestCache,
    /// Where `/admin` audit lines go. `NullEventSink` unless the embedder
    /// supplies its own — the daemon passes the sink that reaches journald.
    pub sink: Arc<dyn EventSink>,
    /// The process manager supervising this host's TES3MP dedicated server,
    /// when the embedder has one. `GET /admin/status` reports
    /// `tes3mpServerRunning: null` without it.
    pub process_manager: Option<Arc<ProcessManager>>,
    /// Serializes every write under `/admin`. The staging routes wait for
    /// it (two uploads must not read and write `pending.json` over each
    /// other); apply takes it *without* waiting and answers 409 when it is
    /// held, because an apply rewrites `data/` and hashes the whole tree and
    /// a queued second one is never what the caller wanted.
    pub admin_write_lock: Arc<tokio::sync::Mutex<()>>,
    /// Whether a running TES3MP server is enforcing a plugin list older than
    /// the served manifest. Set by an apply that finds the game server up
    /// (apply deliberately does not restart it), cleared by a successful
    /// `POST /admin/restart`. Read by `GET /admin/status`.
    pub tes3mp_plugin_list_stale: Arc<AtomicBool>,
    /// Shared with whoever supervises the TES3MP dedicated server: it says
    /// there is one to restart, and it is how an admin-requested stop is told
    /// apart from a crash. The daemon holds the same `Arc` and its death
    /// watcher consults it; an embedder that never calls
    /// [`ServerRestartState::mark_supervising`] gets a `409` from
    /// `POST /admin/restart`, which is the desktop app and a `--sync-only`
    /// daemon.
    pub server_restart: Arc<ServerRestartState>,
}

impl ServerContext {
    /// The player-facing context: no audit sink, no process manager.
    pub fn new(sync_host: SharedSyncHost, manifest_cache: SharedHostingManifestCache) -> Arc<Self> {
        Self::builder(sync_host, manifest_cache).shared()
    }

    /// Same defaults as [`ServerContext::new`], unwrapped so an embedder can
    /// add the `/admin` services before sharing it.
    pub fn builder(sync_host: SharedSyncHost, manifest_cache: SharedHostingManifestCache) -> Self {
        Self {
            sync_host,
            manifest_cache,
            sink: Arc::new(NullEventSink),
            process_manager: None,
            admin_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            tes3mp_plugin_list_stale: Arc::new(AtomicBool::new(false)),
            server_restart: Arc::new(ServerRestartState::new()),
        }
    }

    pub fn with_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn with_process_manager(mut self, process_manager: Arc<ProcessManager>) -> Self {
        self.process_manager = Some(process_manager);
        self
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}
