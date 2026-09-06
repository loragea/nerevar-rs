use std::collections::HashMap;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::openmw_ini_importer::GlobalOpenMwLaunchSession;
use crate::reporter::{emit_event, EventSink};
use crate::sync_client::types::ProcessStatusEvent;

use super::types::ProcessRole;

pub struct ManagedProcess {
    pub child: Arc<Mutex<Option<Child>>>,
    /// When this child was handed to the manager, which is within
    /// milliseconds of when it was spawned. Reported by
    /// `GET /admin/status` as the game server's uptime anchor.
    pub started_at: DateTime<Utc>,
}

pub struct ProcessManager {
    processes: Mutex<HashMap<String, ManagedProcess>>,
    global_openmw_session: Mutex<Option<GlobalOpenMwLaunchSession>>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Mutex::new(HashMap::new()),
            global_openmw_session: Mutex::new(None),
        }
    }

    pub fn store_global_openmw_session(
        &self,
        session: GlobalOpenMwLaunchSession,
    ) -> Result<(), String> {
        let mut guard = self
            .global_openmw_session
            .lock()
            .map_err(|_| "Process manager lock poisoned".to_string())?;
        if guard.is_some() {
            return Err("Global OpenMW config is already swapped for launch".to_string());
        }
        *guard = Some(session);
        Ok(())
    }

    pub fn restore_global_openmw_session_if_any(&self) {
        let session = self
            .global_openmw_session
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        if let Some(session) = session {
            if let Err(err) = crate::openmw_ini_importer::restore_global_openmw_launch(session) {
                log::error!(
                    "Failed to restore global OpenMW config after TES3MP launch: {err}"
                );
            }
        }
    }

    pub fn key(instance_id: &str, role: ProcessRole) -> String {
        format!("{}:{}", instance_id, role.as_str())
    }

    pub fn running_instance_for_role(
        &self,
        role: ProcessRole,
    ) -> Result<Option<String>, String> {
        let suffix = format!(":{}", role.as_str());
        let keys: Vec<String> = {
            let guard = self
                .processes
                .lock()
                .map_err(|_| "Process manager lock poisoned".to_string())?;
            guard
                .keys()
                .filter(|key| key.ends_with(&suffix))
                .cloned()
                .collect()
        };

        for key in keys {
            let Some(instance_id) = key.strip_suffix(&suffix) else {
                continue;
            };
            if self.is_running(instance_id, role)? {
                return Ok(Some(instance_id.to_string()));
            }
        }

        Ok(None)
    }

    pub fn ensure_can_launch(&self, instance_id: &str, role: ProcessRole) -> Result<(), String> {
        if let Some(other) = self.running_instance_for_role(role)? {
            if other != instance_id {
                return Err(format!(
                    "A TES3MP {} is already running for instance \"{other}\". Stop it before launching another.",
                    role.as_str()
                ));
            }
        }
        Ok(())
    }

    pub fn global_status(&self) -> Result<super::types::GlobalProcessStatus, String> {
        Ok(super::types::GlobalProcessStatus {
            client_instance_id: self.running_instance_for_role(ProcessRole::Client)?,
            server_instance_id: self.running_instance_for_role(ProcessRole::Server)?,
        })
    }

    pub fn insert(
        &self,
        instance_id: &str,
        role: ProcessRole,
        child: Child,
    ) -> Result<Arc<Mutex<Option<Child>>>, String> {
        self.ensure_can_launch(instance_id, role)?;

        let key = Self::key(instance_id, role);
        let wrapped = Arc::new(Mutex::new(Some(child)));
        let mut guard = self
            .processes
            .lock()
            .map_err(|_| "Process manager lock poisoned".to_string())?;

        if let Some(existing) = guard.remove(&key) {
            stop_child_in_background(existing.child, None);
        }

        guard.insert(
            key,
            ManagedProcess {
                child: wrapped.clone(),
                started_at: Utc::now(),
            },
        );
        Ok(wrapped)
    }

    /// Signal the process to exit without blocking the caller.
    pub fn stop(
        &self,
        sink: Option<Arc<dyn EventSink>>,
        instance_id: &str,
        role: ProcessRole,
    ) -> Result<bool, String> {
        let key = Self::key(instance_id, role);
        let child_arc = {
            let mut guard = self
                .processes
                .lock()
                .map_err(|_| "Process manager lock poisoned".to_string())?;
            guard.remove(&key).map(|managed| managed.child)
        };

        let Some(child_arc) = child_arc else {
            return Ok(false);
        };

        let instance_id = instance_id.to_string();
        let role_str = role.as_str().to_string();
        if role == ProcessRole::Client {
            self.restore_global_openmw_session_if_any();
        }
        stop_child_in_background(
            child_arc,
            sink.map(|sink| (sink, instance_id, role_str)),
        );
        Ok(true)
    }

    /// Synchronous variant of `stop`: kills and reaps the process on the
    /// calling thread instead of a background thread, so the caller knows
    /// for certain the process (and any port it held) is gone before it
    /// proceeds. `stop` must stay non-blocking for GUI commands (never
    /// stall the Tauri command thread); this exists for the headless
    /// `nerevar-host` daemon's shutdown path, where blocking briefly at
    /// exit — after SIGTERM/SIGINT, before the process itself exits — is
    /// exactly the point (systemd expects the port released before the
    /// unit is considered stopped).
    pub fn stop_blocking(
        &self,
        sink: Option<Arc<dyn EventSink>>,
        instance_id: &str,
        role: ProcessRole,
    ) -> Result<bool, String> {
        let key = Self::key(instance_id, role);
        let child_arc = {
            let mut guard = self
                .processes
                .lock()
                .map_err(|_| "Process manager lock poisoned".to_string())?;
            guard.remove(&key).map(|managed| managed.child)
        };

        let Some(child_arc) = child_arc else {
            return Ok(false);
        };

        if role == ProcessRole::Client {
            self.restore_global_openmw_session_if_any();
        }

        let Some(exit_code) = kill_and_reap(&child_arc) else {
            // Already gone (raced with natural exit); the exit watcher
            // emitted its own process-status, nothing more to report.
            return Ok(true);
        };
        if let Some(sink) = sink {
            emit_event(
                &*sink,
                "process-status",
                &ProcessStatusEvent {
                    instance_id: instance_id.to_string(),
                    role: role.as_str().to_string(),
                    running: false,
                    exit_code,
                },
            );
        }
        Ok(true)
    }

    pub fn remove(&self, instance_id: &str, role: ProcessRole) {
        if let Ok(mut guard) = self.processes.lock() {
            guard.remove(&Self::key(instance_id, role));
        }
    }

    /// The pid of the tracked child for `role`, or `None` when nothing is
    /// tracked. On Linux this is the pid of TES3MP's wrapper script — the same
    /// pid the manager signals as a process group, not a second one — and it
    /// is only meaningful while the child is alive, so callers pair it with
    /// [`ProcessManager::is_running`] or read it right after a launch.
    pub fn pid(&self, instance_id: &str, role: ProcessRole) -> Result<Option<u32>, String> {
        let Some(child_arc) = self.child_handle(instance_id, role)? else {
            return Ok(None);
        };
        let slot = child_arc
            .lock()
            .map_err(|_| "Process child lock poisoned".to_string())?;
        Ok(slot.as_ref().map(|child| child.id()))
    }

    /// When the tracked child for `role` was launched, or `None` when nothing
    /// is tracked.
    pub fn started_at(
        &self,
        instance_id: &str,
        role: ProcessRole,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let guard = self
            .processes
            .lock()
            .map_err(|_| "Process manager lock poisoned".to_string())?;
        Ok(guard
            .get(&Self::key(instance_id, role))
            .map(|managed| managed.started_at))
    }

    /// The child handle for `role`, cloned out from under the map lock so no
    /// caller holds both locks at once.
    fn child_handle(
        &self,
        instance_id: &str,
        role: ProcessRole,
    ) -> Result<Option<Arc<Mutex<Option<Child>>>>, String> {
        let guard = self
            .processes
            .lock()
            .map_err(|_| "Process manager lock poisoned".to_string())?;
        Ok(guard
            .get(&Self::key(instance_id, role))
            .map(|managed| managed.child.clone()))
    }

    /// Report whether the managed child is still alive, WITHOUT taking it.
    ///
    /// This is a pure observation: callers poll it (the GUI's
    /// `is_instance_process_running` command, `nerevar-host`'s supervision
    /// loop, the delete guard), and an observer must never consume the exit.
    /// Only `spawn_exit_watcher` (natural exit) and the stop paths reap a
    /// child, because those are the paths that emit the final
    /// `process-status` and restore the swapped global `openmw.cfg`; if a
    /// poll cleared the slot first, the watcher would see an empty slot,
    /// take it for a `stop()` and return silently — leaving the user's
    /// `openmw.cfg` swapped out for the composed launch config.
    ///
    /// So an exited-but-unreaped child reports `Ok(false)` while its entry
    /// stays in the map. `try_wait` caches the status, so the watcher still
    /// sees the exit on its next tick and cleans up then. Nothing keys off
    /// entry presence — `running_instance_for_role` (and through it
    /// `ensure_can_launch` and `global_status`) asks this method, so a
    /// lingering exited entry never blocks a relaunch.
    pub fn is_running(&self, instance_id: &str, role: ProcessRole) -> Result<bool, String> {
        let key = Self::key(instance_id, role);
        let guard = self
            .processes
            .lock()
            .map_err(|_| "Process manager lock poisoned".to_string())?;

        let Some(managed) = guard.get(&key) else {
            return Ok(false);
        };

        let child_arc = managed.child.clone();
        drop(guard);

        let mut slot = child_arc
            .lock()
            .map_err(|_| "Process child lock poisoned".to_string())?;

        let Some(ref mut child) = *slot else {
            return Ok(false);
        };

        match child.try_wait() {
            // Exited — but leave the child in the slot and the entry in the
            // map for the exit watcher to reap; see the doc comment above.
            Ok(Some(_)) => Ok(false),
            Ok(None) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

/// Best-effort SIGKILL to the whole process group `pid` leads (spawn.rs sets
/// `process_group(0)` on every TES3MP child, so `pid` is also its pgid).
/// Reaches the real ELF binary a wrapper script (tes3mp/tes3mp-server) ran
/// as a plain child rather than `exec`'d into — see the comment on
/// `configure_tes3mp_command` in spawn.rs. Shells out to `kill(1)` rather
/// than a raw `kill(2)` FFI call so this stays dependency-free — core takes
/// no new crate just to send a signal; `kill(1)` is
/// as ubiquitous on Unix as the shell itself. Errors (missing `kill(1)`,
/// already-dead group) are swallowed — the direct `child.kill()` right
/// after this call is still the authoritative, always-available fallback.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pid}"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Kill and reap the child held in `child_arc`, if any. `None` means the
/// slot was already empty (a race with natural exit — the exit watcher
/// already emitted its own status); `Some(code)` means this call did the
/// killing, with `code` its exit code if captured. Shared by the
/// non-blocking (`stop_child_in_background`) and blocking (`stop_blocking`)
/// shutdown paths so both kill the same way.
fn kill_and_reap(child_arc: &Arc<Mutex<Option<Child>>>) -> Option<Option<i32>> {
    let mut slot = child_arc.lock().ok()?;
    let mut child = slot.take()?;
    // A slot can hold an already-exited, already-reaped child: `is_running`
    // reaps with `try_wait` (deliberately, without taking the slot) and a
    // stop can land before the watcher's next tick. Once reaped, the pid is
    // free for the OS to reuse, so signalling its process group could hit an
    // unrelated process. `Child::kill` guards itself against that; the
    // `kill(1)` group kill cannot, so skip both when the child is known
    // gone. `wait()` below then just returns the cached status.
    if !matches!(child.try_wait(), Ok(Some(_))) {
        #[cfg(unix)]
        kill_process_group(child.id());
        let _ = child.kill();
    }
    Some(child.wait().ok().and_then(|s| s.code()))
}

/// Kill and reap a child on a background thread so Tauri commands never block on `wait()`.
/// Only this path (or the watch thread after natural exit) may call `wait()`.
fn stop_child_in_background(
    child_arc: Arc<Mutex<Option<Child>>>,
    status_emit: Option<(Arc<dyn EventSink>, String, String)>,
) {
    thread::spawn(move || {
        let Some(exit_code) = kill_and_reap(&child_arc) else {
            return;
        };

        if let Some((sink, instance_id, role)) = status_emit {
            emit_event(
                &*sink,
                "process-status",
                &ProcessStatusEvent {
                    instance_id,
                    role,
                    running: false,
                    exit_code,
                },
            );
        }
    });
}

/// Poll for process exit without holding the child lock across `wait()`.
pub fn spawn_exit_watcher(
    sink: Arc<dyn EventSink>,
    manager: Arc<ProcessManager>,
    instance_id: String,
    role: ProcessRole,
    child_arc: Arc<Mutex<Option<Child>>>,
) {
    thread::spawn(move || {
        let exit_code = loop {
            thread::sleep(Duration::from_millis(250));

            let status = {
                let mut slot = match child_arc.lock() {
                    Ok(g) => g,
                    Err(_) => break None,
                };
                let Some(ref mut child) = *slot else {
                    // Stopped via stop(); that path emits process-status.
                    return;
                };
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *slot = None;
                        Some(status.code())
                    }
                    Ok(None) => continue,
                    Err(_) => {
                        *slot = None;
                        None
                    }
                }
            };

            if let Some(code) = status {
                break code;
            }
        };

        manager.remove(&instance_id, role);
        if role == ProcessRole::Client {
            manager.restore_global_openmw_session_if_any();
        }
        emit_event(
            &*sink,
            "process-status",
            &ProcessStatusEvent {
                instance_id,
                role: role.as_str().to_string(),
                running: false,
                exit_code,
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::CollectingEventSink;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const DEADLINE: Duration = Duration::from_secs(10);

    /// A child that exits on its own, immediately. Written per-platform
    /// rather than `cfg(unix)`-gated so the tests also run on the Windows
    /// build; `true` and `cmd /C exit 0` are the shortest-lived programs
    /// each platform is guaranteed to have.
    fn short_lived_child() -> Child {
        #[cfg(unix)]
        let mut command = Command::new("true");
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/C", "exit", "0"]);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn short-lived child")
    }

    /// Poll `is_running` until it reports the child gone — this is exactly
    /// what the GUI's `is_instance_process_running` command and
    /// `nerevar-host`'s supervision loop do, and it is the observation that
    /// used to steal the exit from the watcher.
    fn poll_until_not_running(manager: &ProcessManager, instance_id: &str, role: ProcessRole) {
        let start = Instant::now();
        while start.elapsed() < DEADLINE {
            if !manager.is_running(instance_id, role).expect("is_running") {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("child never reported as exited");
    }

    fn entry_present(manager: &ProcessManager, instance_id: &str, role: ProcessRole) -> bool {
        manager
            .processes
            .lock()
            .unwrap()
            .contains_key(&ProcessManager::key(instance_id, role))
    }

    fn child_slot_occupied(manager: &ProcessManager, instance_id: &str, role: ProcessRole) -> bool {
        let guard = manager.processes.lock().unwrap();
        let managed = guard
            .get(&ProcessManager::key(instance_id, role))
            .expect("entry present");
        let occupied = managed.child.lock().unwrap().is_some();
        occupied
    }

    fn wait_for_events(sink: &CollectingEventSink) -> Vec<(&'static str, serde_json::Value)> {
        let start = Instant::now();
        loop {
            let events = sink.events();
            if !events.is_empty() {
                return events;
            }
            if start.elapsed() >= DEADLINE {
                return events;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn is_running_reports_exit_without_reaping_the_entry() {
        let manager = ProcessManager::new();
        manager
            .insert("inst", ProcessRole::Client, short_lived_child())
            .expect("insert");

        poll_until_not_running(&manager, "inst", ProcessRole::Client);

        assert!(
            entry_present(&manager, "inst", ProcessRole::Client),
            "is_running must not remove the map entry — the watcher still needs it"
        );
        assert!(
            child_slot_occupied(&manager, "inst", ProcessRole::Client),
            "is_running must not empty the child slot — an empty slot reads as stop()"
        );
        // Repeated polling stays stable: `try_wait` hands back the cached status.
        assert!(!manager.is_running("inst", ProcessRole::Client).unwrap());
        assert!(entry_present(&manager, "inst", ProcessRole::Client));
    }

    #[test]
    fn exit_watcher_still_emits_after_a_poll_saw_the_exit() {
        let sink = Arc::new(CollectingEventSink::default());
        let manager = Arc::new(ProcessManager::new());
        let child_handle = manager
            .insert("inst", ProcessRole::Client, short_lived_child())
            .expect("insert");
        spawn_exit_watcher(
            sink.clone(),
            manager.clone(),
            "inst".to_string(),
            ProcessRole::Client,
            child_handle,
        );

        // Race the watcher's first tick, the way a GUI poll does.
        poll_until_not_running(&manager, "inst", ProcessRole::Client);

        let events = wait_for_events(&sink);
        assert_eq!(
            events.len(),
            1,
            "expected exactly one process-status from the watcher, got {events:?}"
        );
        let (name, payload) = &events[0];
        assert_eq!(*name, "process-status");
        assert_eq!(payload["instanceId"], "inst");
        assert_eq!(payload["role"], "client");
        assert_eq!(payload["running"], false);
        assert_eq!(payload["exitCode"], 0);

        // The watcher, not the poll, is what clears the entry.
        let start = Instant::now();
        while entry_present(&manager, "inst", ProcessRole::Client) {
            assert!(
                start.elapsed() < DEADLINE,
                "watcher never removed the entry"
            );
            thread::sleep(Duration::from_millis(10));
        }

        // The watcher also runs `restore_global_openmw_session_if_any` on
        // this (client) path — the call that puts the user's own
        // `openmw.cfg` back. It is a no-op here because no session is
        // stored: `GlobalOpenMwLaunchSession` has private fields and is only
        // built by `begin_global_openmw_launch`, which reads and writes the
        // real per-user OpenMW config directory. Covering the restore itself
        // therefore needs an end-to-end launch, not a unit test; what is
        // pinned here is that the watcher reaches that code at all, which is
        // precisely what the stolen-exit bug prevented.
        assert!(manager.global_openmw_session.lock().unwrap().is_none());
    }

    #[test]
    fn an_exited_child_never_blocks_the_next_launch() {
        let manager = ProcessManager::new();
        manager
            .insert("inst", ProcessRole::Server, short_lived_child())
            .expect("insert");
        poll_until_not_running(&manager, "inst", ProcessRole::Server);

        // No watcher here, so the exited entry lingers for the whole test —
        // the worst case of the window this fix opens.
        assert!(entry_present(&manager, "inst", ProcessRole::Server));
        assert!(manager
            .running_instance_for_role(ProcessRole::Server)
            .unwrap()
            .is_none());
        manager
            .ensure_can_launch("other", ProcessRole::Server)
            .expect("a dead process must not reserve the role");
        assert!(manager
            .global_status()
            .unwrap()
            .server_instance_id
            .is_none());

        // Relaunching the same instance replaces the stale entry.
        manager
            .insert("inst", ProcessRole::Server, short_lived_child())
            .expect("relaunch");
        assert!(entry_present(&manager, "inst", ProcessRole::Server));
    }

    #[test]
    fn stop_after_an_observed_exit_still_reports_and_clears() {
        let sink = Arc::new(CollectingEventSink::default());
        let manager = ProcessManager::new();
        manager
            .insert("inst", ProcessRole::Server, short_lived_child())
            .expect("insert");
        poll_until_not_running(&manager, "inst", ProcessRole::Server);

        // stop_blocking on an exited-but-unreaped child: it must not signal
        // the (now reusable) pid, and it still reports the exit.
        assert!(manager
            .stop_blocking(Some(sink.clone()), "inst", ProcessRole::Server)
            .expect("stop_blocking"));
        let events = sink.events();
        assert_eq!(
            events.len(),
            1,
            "expected one process-status, got {events:?}"
        );
        assert_eq!(events[0].1["running"], false);
        assert_eq!(events[0].1["exitCode"], 0);
        assert!(!entry_present(&manager, "inst", ProcessRole::Server));
    }
}
