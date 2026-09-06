use std::sync::Arc;

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
