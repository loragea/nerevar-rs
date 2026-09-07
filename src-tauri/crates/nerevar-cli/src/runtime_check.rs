//! `nerevar-cli runtime-check <install-dir>` — can this TES3MP runtime start?
//!
//! The same check `runtime::acquire` runs after every install, on its own, for
//! the runtime a player already has. A Linux tarball with every file in place
//! still fails to launch when the machine is missing a shared library it links
//! against, and this is how someone on a headless box finds out which packages
//! to install without reading a launch error out of a GUI.
//!
//! Exit codes: 0 when the runtime starts (or when there was nothing to check —
//! a non-Linux host has no such failure mode), 1 when it cannot.

use std::io::Write;
use std::path::Path;

use nerevar_core::runtime::{check_runtime_health, RuntimeHealthStatus, TargetPlatform};

/// Runs the check and writes its verdict to `out`, returning the process exit
/// code.
///
/// The paragraph goes out on its own line first, because that is the whole
/// answer; the executable that was probed and any captured output follow it
/// for someone who has to dig.
pub fn run(install_dir: &Path, out: &mut impl Write) -> i32 {
    let health = check_runtime_health(install_dir, TargetPlatform::current());

    let _ = writeln!(out, "{}", health.summary());
    if let Some(exe) = &health.checked_executable {
        let _ = writeln!(out, "checked: {exe}");
    }
    if let Some(reason) = &health.reason {
        if health.status == RuntimeHealthStatus::Failed {
            let _ = writeln!(out, "reason: {reason}");
        }
    }
    if let Some(output) = &health.output {
        if !output.is_empty() {
            let _ = writeln!(out, "--- output ---\n{output}");
        }
    }

    match health.status {
        RuntimeHealthStatus::Failed => 1,
        RuntimeHealthStatus::Healthy | RuntimeHealthStatus::NotChecked => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "nerevar-cli-runtime-check-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    #[cfg(unix)]
    fn fake_runtime(dir: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let wrapper = dir.join("tes3mp");
        fs::write(&wrapper, body).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("tes3mp.x86_64"), b"stub").unwrap();
    }

    /// A directory that is not a runtime at all is "nothing to check", not a
    /// failure: exiting non-zero there would make the command useless in a
    /// script that runs it opportunistically.
    #[test]
    fn a_directory_with_no_runtime_exits_zero() {
        let dir = scratch("empty");
        let mut out = Vec::new();
        assert_eq!(run(&dir.0, &mut out), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("not health-checked"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_healthy_runtime_exits_zero_and_prints_its_version() {
        let dir = scratch("healthy");
        fake_runtime(&dir.0, "#!/bin/sh\necho 'OpenMW version 0.47.0'\n");
        let mut out = Vec::new();
        assert_eq!(run(&dir.0, &mut out), 0);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("check passed"), "{text}");
        assert!(text.contains("0.47.0"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_runtime_missing_a_library_exits_one_and_names_the_package() {
        let dir = scratch("broken");
        fake_runtime(
            &dir.0,
            "#!/bin/sh\necho '\tlibopenal.so.1 => not found' >&2\nexit 127\n",
        );
        let mut out = Vec::new();
        assert_eq!(run(&dir.0, &mut out), 1);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("TES3MP cannot start"), "{text}");
        assert!(text.contains("libopenal.so.1"), "{text}");
        assert!(text.contains("libopenal1"), "{text}");
        assert!(text.contains("--- output ---"), "{text}");
    }
}
