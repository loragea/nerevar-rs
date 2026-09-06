//! Restarting the TES3MP dedicated server on an admin's word, and the one
//! piece of state that keeps the daemon from mistaking that for a crash.
//!
//! `nerevar-host` watches its TES3MP child and exits 69 the moment it is gone,
//! so that a `Restart=on-failure` unit brings the whole service back rather
//! than leaving sync hosting up with nobody able to play. A restart stops that
//! same child on purpose, which without help reads exactly like a death.
//!
//! [`ServerRestartState`] is the help: a restart holds a [`RestartWindow`]
//! across the stop and the relaunch, and the watcher takes a [`RestartMark`]
//! *before* each liveness check and asks [`ServerRestartState::is_unrequested_death`]
//! about the reading afterwards. A window that was open at any point in that
//! span — still open, or opened and closed inside it — makes the reading
//! inconclusive, so the watcher keeps waiting. Every other not-running reading
//! is a real death, including one right after a restart: the window is closed
//! by then and the completed count has stopped moving.
//!
//! Nothing here is a timeout or a grace period. The window is exactly as long
//! as the restart, and if the relaunch fails the window still closes — the
//! server really is gone, and the watcher's next tick says so.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Serialize;

use crate::process_manager::{launch_tes3mp_server, ProcessManager, ProcessRole};
use crate::reporter::EventSink;

/// Tracks admin-requested restarts of the TES3MP dedicated server.
///
/// Shared between the embedder that supervises the game server and the
/// `/admin` routes, through
/// [`ServerContext`](crate::nerevar_server::state::ServerContext).
#[derive(Debug, Default)]
pub struct ServerRestartState {
    /// How many restarts are between their stop and their relaunch right now.
    /// A count rather than a flag so two overlapping restarts cannot close
    /// each other's window; in practice the admin write lock keeps it at 0 or
    /// 1.
    in_flight: AtomicUsize,
    /// How many restart windows have closed. Lets a watcher tell "no restart
    /// happened while I was looking" from "a whole restart slipped through the
    /// gap between my mark and my reading".
    completed: AtomicU64,
    /// Whether this embedder launched a TES3MP dedicated server it could
    /// restart. False on the desktop app, and on a `--sync-only` daemon, which
    /// is why `POST /admin/restart` answers those with a 409 instead of
    /// launching a game server nobody asked for.
    supervising: AtomicBool,
}

/// A reading of [`ServerRestartState`] taken *before* a liveness check, to be
/// handed back to [`ServerRestartState::is_unrequested_death`] with the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartMark(u64);

impl ServerRestartState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a restart window. Held for the whole stop-and-relaunch, and
    /// closed by dropping the returned guard — including on an error path or a
    /// panic, so a failed restart can never wedge the watcher into ignoring a
    /// dead server forever.
    pub fn begin(self: &Arc<Self>) -> RestartWindow {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        RestartWindow {
            state: Arc::clone(self),
        }
    }

    /// Take this before checking whether the server is running.
    pub fn mark(&self) -> RestartMark {
        RestartMark(self.completed.load(Ordering::SeqCst))
    }

    /// Whether a "not running" reading taken after `mark` was an unrequested
    /// death rather than an admin restart passing through.
    ///
    /// The two loads are in this order on purpose: a restart that ends between
    /// them has already bumped `completed` (the guard's drop increments it
    /// before decrementing `in_flight`), so it is still caught by the second
    /// load. The other order could see a quiet count and a quiet flag for a
    /// restart that was in fact in progress.
    pub fn is_unrequested_death(&self, mark: RestartMark) -> bool {
        self.in_flight.load(Ordering::SeqCst) == 0
            && self.completed.load(Ordering::SeqCst) == mark.0
    }

    /// Whether a restart is between its stop and its relaunch right now.
    pub fn restart_in_flight(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) != 0
    }

    /// How many restarts have finished, successfully or not.
    pub fn completed_restarts(&self) -> u64 {
        self.completed.load(Ordering::SeqCst)
    }

    /// Declare that this embedder launched the TES3MP dedicated server and can
    /// restart it. The daemon calls this once, after the launch at boot.
    pub fn mark_supervising(&self) {
        self.supervising.store(true, Ordering::SeqCst);
    }

    /// Whether `POST /admin/restart` has a game server to restart at all.
    pub fn is_supervising(&self) -> bool {
        self.supervising.load(Ordering::SeqCst)
    }
}

/// An open restart window; see [`ServerRestartState::begin`].
#[derive(Debug)]
pub struct RestartWindow {
    state: Arc<ServerRestartState>,
}

impl Drop for RestartWindow {
    fn drop(&mut self) {
        // `completed` first: a watcher reads `in_flight` before `completed`,
        // so bumping the count before clearing the flag means it can never see
        // both quiet for a restart that just happened.
        self.state.completed.fetch_add(1, Ordering::SeqCst);
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What a restart did, as `POST /admin/restart` reports it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartOutcome {
    /// Always true on the success path; the field exists so the body reads as
    /// an answer rather than as bare metadata.
    pub restarted: bool,
    /// The new child's pid, or `None` if the process manager cannot say. On
    /// Linux this is the pid of TES3MP's wrapper script, which is also the
    /// process group the daemon signals — the same pid the daemon has always
    /// tracked, not a second one.
    pub pid: Option<u32>,
    /// RFC 3339 instant the new child was launched.
    pub started_at: Option<String>,
    /// Whether a server was actually running to stop. `false` means the
    /// restart started one that was already gone, which is worth seeing.
    pub was_running: bool,
}

/// Stop the TES3MP dedicated server and start it again the way the daemon
/// started it at boot.
///
/// Blocking on purpose: the stop reaps the process group so the game port is
/// free before the relaunch binds it, and the relaunch rewrites the launch
/// config and `requiredDataFiles.json` from what is on disk now — which is how
/// a restart picks up an apply's new plugin list. Callers on an async runtime
/// must put this on a blocking thread.
///
/// The restart window is open for the whole call, so the daemon's death
/// watcher does not read the intentional stop as a crash.
pub fn restart_tes3mp_server(
    sink: Arc<dyn EventSink>,
    manager: &Arc<ProcessManager>,
    restart_state: &Arc<ServerRestartState>,
    instance_id: &str,
    instance_root: &Path,
    data_dir: &Path,
) -> Result<RestartOutcome, String> {
    let _window = restart_state.begin();

    let was_running =
        manager.stop_blocking(Some(sink.clone()), instance_id, ProcessRole::Server)?;
    if !was_running {
        log::warn!(
            "Restarting the TES3MP dedicated server for \"{instance_id}\", which was not running"
        );
    }

    launch_tes3mp_server(
        sink,
        Arc::clone(manager),
        instance_id,
        instance_root,
        data_dir,
    )?;

    Ok(RestartOutcome {
        restarted: true,
        pid: manager.pid(instance_id, ProcessRole::Server).ok().flatten(),
        started_at: manager
            .started_at(instance_id, ProcessRole::Server)
            .ok()
            .flatten()
            .map(|started| started.to_rfc3339()),
        was_running,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_state_calls_a_stopped_server_a_death() {
        let state = Arc::new(ServerRestartState::new());
        let mark = state.mark();
        assert!(!state.restart_in_flight());
        assert!(state.is_unrequested_death(mark));
        assert_eq!(state.completed_restarts(), 0);
    }

    #[test]
    fn an_open_window_makes_a_stopped_server_inconclusive() {
        let state = Arc::new(ServerRestartState::new());
        let mark = state.mark();
        let window = state.begin();

        assert!(state.restart_in_flight());
        // Both the mark taken before the restart and one taken during it.
        assert!(!state.is_unrequested_death(mark));
        assert!(!state.is_unrequested_death(state.mark()));

        drop(window);
        assert!(!state.restart_in_flight());
        assert_eq!(state.completed_restarts(), 1);
    }

    /// The gap the mark exists for: a whole restart between the watcher's mark
    /// and its reading leaves nothing to see in `in_flight` alone.
    #[test]
    fn a_restart_that_began_and_ended_since_the_mark_is_not_a_death() {
        let state = Arc::new(ServerRestartState::new());
        let mark = state.mark();
        drop(state.begin());

        assert!(!state.restart_in_flight());
        assert!(!state.is_unrequested_death(mark));
    }

    /// The requirement that makes this worth having: after a restart, the
    /// daemon must still exit 69 if the new process dies.
    #[test]
    fn a_death_right_after_a_restart_is_still_a_death() {
        let state = Arc::new(ServerRestartState::new());
        drop(state.begin());

        let mark = state.mark();
        assert!(state.is_unrequested_death(mark));
    }

    /// Overlapping windows must not close each other: a count, not a flag.
    #[test]
    fn two_windows_only_reopen_when_both_are_closed() {
        let state = Arc::new(ServerRestartState::new());
        let first = state.begin();
        let second = state.begin();
        assert!(state.restart_in_flight());

        drop(first);
        assert!(
            state.restart_in_flight(),
            "one restart finishing must not clear another's window"
        );
        assert!(!state.is_unrequested_death(state.mark()));

        drop(second);
        assert!(!state.restart_in_flight());
        assert_eq!(state.completed_restarts(), 2);
    }

    /// A panicking restart still closes its window, so a dead server is
    /// noticed rather than ignored forever.
    #[test]
    fn a_panicking_restart_still_closes_its_window() {
        let state = Arc::new(ServerRestartState::new());
        let panicking = Arc::clone(&state);
        let result = std::panic::catch_unwind(move || {
            let _window = panicking.begin();
            panic!("restart blew up");
        });

        assert!(result.is_err());
        assert!(!state.restart_in_flight());
        assert!(state.is_unrequested_death(state.mark()));
    }

    #[test]
    fn nothing_is_supervised_until_the_embedder_says_so() {
        let state = ServerRestartState::new();
        assert!(!state.is_supervising());
        state.mark_supervising();
        assert!(state.is_supervising());
    }
}
