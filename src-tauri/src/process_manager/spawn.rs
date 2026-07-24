use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::instance_data::{
    launch_cfg_path, launch_settings_overlay_path, resolve_instance_openmw_config,
    write_instance_launch_cfg,
};
use crate::instance_setup::{instance_tes3mp_dir, write_required_data_files_for_resolved};
use crate::openmw_ini_importer::begin_global_openmw_launch;
use crate::reporter::{emit_event, EventSink};
use crate::sync_client::types::{ProcessOutputEvent, ProcessStatusEvent, ProcessStream};

use super::state::{spawn_exit_watcher, ProcessManager};
use super::types::ProcessRole;

// Order matters: within a directory, the first matching name wins. Linux tarball
// wrapper scripts (which set LD_LIBRARY_PATH before exec'ing the real binary) must
// be preferred over the raw .x86_64 ELF binaries they wrap.
const CLIENT_EXE_NAMES: &[&str] = &[
    "tes3mp.exe",
    "openmw.exe",
    "tes3mp",
    "openmw",
    "tes3mp.x86_64",
    "openmw.x86_64",
];
const SERVER_EXE_NAMES: &[&str] = &["tes3mp-server.exe", "tes3mp-server", "tes3mp-server.x86_64"];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn configure_tes3mp_command(command: &mut Command, working_dir: &Path) {
    command
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Console-subsystem children otherwise get a blank terminal when launched from
    // our GUI app. Output is still captured via the pipes above.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

pub fn find_executable(root: &Path, names: &[&str], max_depth: u32) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }

    let mut best: Option<(usize, PathBuf)> = None;
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let rank = names.iter().position(|name| file_name.eq_ignore_ascii_case(name));
            if let Some(rank) = rank {
                if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
                    best = Some((rank, path));
                }
            }
        }
    }
    if let Some((_, path)) = best {
        return Some(path);
    }

    if max_depth > 1 {
        let entries = std::fs::read_dir(root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_executable(&path, names, max_depth - 1) {
                    return Some(found);
                }
            }
        }
    }

    None
}

fn pipe_process_output(
    sink: Arc<dyn EventSink>,
    instance_id: String,
    role: ProcessRole,
    stream: ProcessStream,
    reader: impl BufRead + Send + 'static,
) {
    thread::spawn(move || {
        for line in reader.lines().map_while(Result::ok) {
            emit_event(
                &*sink,
                "process-output",
                &ProcessOutputEvent {
                    instance_id: instance_id.clone(),
                    role: role.as_str().to_string(),
                    stream: stream.clone(),
                    line,
                },
            );
        }
    });
}

    fn prepare_launch_cfg(data_dir: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let resolved = resolve_instance_openmw_config(data_dir)?;
    let settings = crate::instance_settings::load_instance_settings(data_dir)?;
    write_instance_launch_cfg(data_dir, &resolved, &settings.openmw_cfg_overrides)?;
    let settings_overlay = launch_settings_overlay_path(data_dir);
    let settings_path = settings_overlay.is_file().then_some(settings_overlay);
    Ok((launch_cfg_path(data_dir), settings_path))
}

pub fn launch_tes3mp_client(
    sink: Arc<dyn EventSink>,
    manager: Arc<ProcessManager>,
    instance_id: &str,
    instance_root: &Path,
    data_dir: &Path,
) -> Result<(), String> {
    let (launch_cfg, launch_settings) = match prepare_launch_cfg(data_dir) {
        Ok(paths) => paths,
        Err(err) => return Err(err),
    };

    let global_session = match begin_global_openmw_launch(&launch_cfg, launch_settings.as_deref()) {
        Ok(session) => session,
        Err(err) => return Err(err),
    };
    manager.store_global_openmw_session(global_session)?;

    let tes3mp_dir = instance_tes3mp_dir(instance_root);
    let exe = find_executable(&tes3mp_dir, CLIENT_EXE_NAMES, 5).ok_or_else(|| {
        manager.restore_global_openmw_session_if_any();
        format!(
            "Could not find TES3MP client executable under {}",
            tes3mp_dir.display()
        )
    })?;

    let mut command = Command::new(&exe);
    configure_tes3mp_command(
        &mut command,
        exe.parent().unwrap_or(&tes3mp_dir),
    );

    log::info!(
        "Launching TES3MP client using global openmw.cfg swap and launch overlay at {}",
        launch_cfg.display()
    );

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            manager.restore_global_openmw_session_if_any();
            return Err(format!("Failed to launch TES3MP client: {err}"));
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    if let Some(out) = stdout {
        pipe_process_output(
            sink.clone(),
            instance_id.to_string(),
            ProcessRole::Client,
            ProcessStream::Stdout,
            BufReader::new(out),
        );
    }
    if let Some(err) = stderr {
        pipe_process_output(
            sink.clone(),
            instance_id.to_string(),
            ProcessRole::Client,
            ProcessStream::Stderr,
            BufReader::new(err),
        );
    }

    thread::sleep(Duration::from_millis(900));
    if let Ok(Some(status)) = child.try_wait() {
        manager.restore_global_openmw_session_if_any();
        return Err(format!(
            "TES3MP client exited immediately (code {:?}). Check the client output console below.",
            status.code()
        ));
    }

    emit_event(
        &*sink,
        "process-status",
        &ProcessStatusEvent {
            instance_id: instance_id.to_string(),
            role: ProcessRole::Client.as_str().to_string(),
            running: true,
            exit_code: None,
        },
    );

    let child_handle = manager.insert(instance_id, ProcessRole::Client, child)?;
    spawn_exit_watcher(
        sink.clone(),
        manager.clone(),
        instance_id.to_string(),
        ProcessRole::Client,
        child_handle,
    );
    Ok(())
}

pub fn launch_tes3mp_server(
    sink: Arc<dyn EventSink>,
    manager: Arc<ProcessManager>,
    instance_id: &str,
    instance_root: &Path,
    data_dir: &Path,
) -> Result<(), String> {
    let (launch_cfg, _launch_settings) = prepare_launch_cfg(data_dir)?;
    let tes3mp_dir = instance_tes3mp_dir(instance_root);
    let resolved = resolve_instance_openmw_config(data_dir)?;
    write_required_data_files_for_resolved(&tes3mp_dir, &resolved)?;
    let exe = find_executable(&tes3mp_dir, SERVER_EXE_NAMES, 5).ok_or_else(|| {
        format!(
            "Could not find TES3MP server executable under {}",
            tes3mp_dir.display()
        )
    })?;

    let mut command = Command::new(&exe);
    configure_tes3mp_command(
        &mut command,
        exe.parent().unwrap_or(&tes3mp_dir),
    );

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to launch TES3MP server: {e}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    if let Some(out) = stdout {
        pipe_process_output(
            sink.clone(),
            instance_id.to_string(),
            ProcessRole::Server,
            ProcessStream::Stdout,
            BufReader::new(out),
        );
    }
    if let Some(err) = stderr {
        pipe_process_output(
            sink.clone(),
            instance_id.to_string(),
            ProcessRole::Server,
            ProcessStream::Stderr,
            BufReader::new(err),
        );
    }

    emit_event(
        &*sink,
        "process-status",
        &ProcessStatusEvent {
            instance_id: instance_id.to_string(),
            role: ProcessRole::Server.as_str().to_string(),
            running: true,
            exit_code: None,
        },
    );

    let child_handle = manager.insert(instance_id, ProcessRole::Server, child)?;
    spawn_exit_watcher(
        sink.clone(),
        manager.clone(),
        instance_id.to_string(),
        ProcessRole::Server,
        child_handle,
    );
    Ok(())
}

pub fn stop_tes3mp_process(
    sink: Arc<dyn EventSink>,
    manager: &ProcessManager,
    instance_id: &str,
    role: ProcessRole,
) -> Result<bool, String> {
    manager.stop(Some(sink), instance_id, role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-find-executable-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prefers_wrapper_script_over_x86_64_binary() {
        let dir = scratch_dir("wrapper-over-binary");
        fs::write(dir.join("tes3mp"), b"#!/bin/sh\n").unwrap();
        fs::write(dir.join("tes3mp.x86_64"), b"elf").unwrap();

        let found = find_executable(&dir, CLIENT_EXE_NAMES, 5).unwrap();
        assert_eq!(found, dir.join("tes3mp"));
    }

    #[test]
    fn falls_back_to_x86_64_binary_when_no_wrapper() {
        let dir = scratch_dir("binary-only");
        fs::write(dir.join("tes3mp.x86_64"), b"elf").unwrap();

        let found = find_executable(&dir, CLIENT_EXE_NAMES, 5).unwrap();
        assert_eq!(found, dir.join("tes3mp.x86_64"));
    }

    #[test]
    fn still_finds_windows_exe() {
        let dir = scratch_dir("windows-exe");
        fs::write(dir.join("tes3mp.exe"), b"pe").unwrap();

        let found = find_executable(&dir, CLIENT_EXE_NAMES, 5).unwrap();
        assert_eq!(found, dir.join("tes3mp.exe"));
    }

    #[test]
    fn finds_executable_in_nested_subdirectory() {
        let dir = scratch_dir("nested");
        let nested = dir.join("bin").join("linux");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("tes3mp-server.x86_64"), b"elf").unwrap();

        let found = find_executable(&dir, SERVER_EXE_NAMES, 5).unwrap();
        assert_eq!(found, nested.join("tes3mp-server.x86_64"));
    }
}
