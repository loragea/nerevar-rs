//! Whether an installed TES3MP runtime can actually start on *this* machine.
//!
//! [`inspect`](super::inspect) answers a different question: are the files
//! there. A Linux tarball can pass that check completely and still be unable
//! to run, because the build links against shared libraries the player's
//! distribution does not ship (OpenAL, SDL2, Boost, the ffmpeg `libav*`
//! family, LuaJIT, Bullet, MyGUI, OpenSceneGraph…) and the loader only
//! notices at exec time. Without this module that shows up as "TES3MP client
//! exited immediately" with no clue what to install.
//!
//! The check is two probes, in order:
//!
//! 1. Run the client the way the launch path would — through the wrapper
//!    script, because the wrapper is what sets `LD_LIBRARY_PATH` and
//!    `OSG_LIBRARY_PATH` — with `--version`, a hard timeout, and a minimal
//!    environment with no `DISPLAY`. Exit 0 plus a version string in the
//!    output is a healthy runtime, and it is the only probe that has to
//!    succeed.
//! 2. When that fails for any reason, run `ldd` against the real ELF binary
//!    the wrapper execs, with the wrapper's library directory applied, and
//!    turn every `=> not found` soname into a per-distribution package hint.
//!
//! `SDL_VIDEODRIVER=dummy` is part of probe 1's environment on purpose:
//! OpenMW 0.47-era builds (official TES3MP 0.8.1) initialise SDL video before
//! they print `--version` and abort with "Could not initialize SDL! No
//! available video device" on a headless box, which would report every
//! perfectly good runtime on a server as broken. Newer builds (the
//! MundusPatensMP 0.51 fork) do not need it but are unharmed by it.
//!
//! Only Linux is checked. Windows and macOS report [`RuntimeHealthStatus::NotChecked`]:
//! neither platform has the bundled-tarball-versus-system-libraries problem
//! this exists for, and neither has `ldd`.

// Everything below the result types is the Linux probe, so most of this
// module's machinery is `#[cfg(unix)]`. A Windows build still compiles the
// whole file — it just gets the stub `check_linux_runtime` and the parsing,
// summary and package-table code, all of which are host-independent.
use std::path::Path;
#[cfg(any(unix, test))]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[cfg(unix)]
use crate::process_manager::spawn::{find_executable, CLIENT_EXE_NAMES};

use super::source::TargetPlatform;

/// How deep under the runtime root the client executable is searched for.
/// The same depth `inspect` and the launch path use, so all three agree on
/// which file is "the client".
#[cfg(unix)]
const EXE_SEARCH_DEPTH: u32 = 5;

/// How long the `--version` probe may take before it is killed. Generous:
/// the binaries are 20–40MB and a cold page-in on a slow disk is not a fault.
#[cfg(unix)]
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// `ldd` only reads the ELF headers and runs the loader in trace mode, so it
/// has no reason to be slow; the timeout is only there so a wedged loader
/// cannot hang an install.
#[cfg(unix)]
const LDD_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-stream cap on captured output. Both streams are kept, so a failure
/// report carries at most twice this many characters of program output.
#[cfg(unix)]
const OUTPUT_LIMIT: usize = 2000;

/// What the health check concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RuntimeHealthStatus {
    /// The client ran and printed its version. It starts on this machine.
    Healthy,
    /// The client did not run. `reason` says why and `missing_libraries`
    /// usually says what to install.
    Failed,
    /// No check was attempted; `reason` says why.
    NotChecked,
}

/// One shared library the loader could not resolve, with the package to
/// install for it on the three distribution families Nerevar's players
/// actually run.
///
/// Hints are *hints*: a family's package name is stable enough to search for
/// even when the exact binary package carries a version suffix that tracks
/// the distribution release. An unrecognised soname keeps all three `None`
/// rather than being guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MissingLibrary {
    /// The soname exactly as the loader asked for it, e.g. `libopenal.so.1`.
    pub soname: String,
    pub debian: Option<String>,
    pub fedora: Option<String>,
    pub arch: Option<String>,
}

impl MissingLibrary {
    /// `libopenal.so.1 (Debian/Ubuntu: libopenal1, Fedora: openal-soft, Arch: openal)`,
    /// or the bare soname plus a note when nothing in the table matched.
    pub fn describe(&self) -> String {
        let mut hints = Vec::new();
        if let Some(debian) = &self.debian {
            hints.push(format!("Debian/Ubuntu: {debian}"));
        }
        if let Some(fedora) = &self.fedora {
            hints.push(format!("Fedora: {fedora}"));
        }
        if let Some(arch) = &self.arch {
            hints.push(format!("Arch: {arch}"));
        }
        if hints.is_empty() {
            format!(
                "{} (no package hint — search your distribution for it)",
                self.soname
            )
        } else {
            format!("{} ({})", self.soname, hints.join(", "))
        }
    }
}

/// The result of a runtime health check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RuntimeHealth {
    pub status: RuntimeHealthStatus,
    /// The version line the client printed, when it printed one.
    pub version: Option<String>,
    /// Why the check failed, or why it was not run. `None` when healthy.
    pub reason: Option<String>,
    /// What the client (or `ldd`) actually printed, truncated. `None` when
    /// healthy — nobody needs the version banner twice.
    pub output: Option<String>,
    /// Sonames the loader could not resolve, with package hints.
    pub missing_libraries: Vec<MissingLibrary>,
    /// Symbol versions this build needs that the system's glibc does not
    /// provide, e.g. `GLIBC_2.38`. A too-old glibc is not a missing package
    /// and must not be reported as one — there is nothing to install.
    pub glibc_requirements: Vec<String>,
    /// The executable that was probed, when one was found.
    pub checked_executable: Option<String>,
}

impl RuntimeHealth {
    #[cfg(unix)]
    fn healthy(version: Option<String>, exe: &Path) -> Self {
        Self {
            status: RuntimeHealthStatus::Healthy,
            version,
            reason: None,
            output: None,
            missing_libraries: Vec::new(),
            glibc_requirements: Vec::new(),
            checked_executable: Some(exe.display().to_string()),
        }
    }

    /// A check that was deliberately not run: the wrong platform, or a host
    /// that cannot run one.
    pub fn not_checked(reason: impl Into<String>) -> Self {
        Self {
            status: RuntimeHealthStatus::NotChecked,
            version: None,
            reason: Some(reason.into()),
            output: None,
            missing_libraries: Vec::new(),
            glibc_requirements: Vec::new(),
            checked_executable: None,
        }
    }

    #[cfg(any(unix, test))]
    fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: RuntimeHealthStatus::Failed,
            version: None,
            reason: Some(reason.into()),
            output: None,
            missing_libraries: Vec::new(),
            glibc_requirements: Vec::new(),
            checked_executable: None,
        }
    }

    pub fn is_failed(&self) -> bool {
        self.status == RuntimeHealthStatus::Failed
    }

    pub fn is_healthy(&self) -> bool {
        self.status == RuntimeHealthStatus::Healthy
    }

    /// One paragraph naming precisely what is wrong, for an operation-complete
    /// banner, a CLI line, or a launch error.
    pub fn summary(&self) -> String {
        match self.status {
            RuntimeHealthStatus::Healthy => match &self.version {
                Some(version) => format!("TES3MP runtime check passed ({version})."),
                None => "TES3MP runtime check passed.".to_string(),
            },
            RuntimeHealthStatus::NotChecked => {
                let reason = self.reason.as_deref().unwrap_or("no reason recorded");
                format!("TES3MP runtime not health-checked: {reason}.")
            }
            RuntimeHealthStatus::Failed => {
                let mut text = String::from("TES3MP cannot start");
                if !self.missing_libraries.is_empty() {
                    let listed: Vec<String> = self
                        .missing_libraries
                        .iter()
                        .map(MissingLibrary::describe)
                        .collect();
                    text.push_str(": missing ");
                    text.push_str(&listed.join(", "));
                    text.push('.');
                } else if !self.glibc_requirements.is_empty() {
                    text.push_str(&format!(
                        ": this build needs a newer C library than this system has (it requires {}). \
                         There is no package to install — use a build made for your distribution, \
                         or upgrade it.",
                        self.glibc_requirements.join(", ")
                    ));
                } else {
                    let reason = self.reason.as_deref().unwrap_or("no reason recorded");
                    text.push_str(&format!(": {reason}."));
                }
                if !self.missing_libraries.is_empty() && !self.glibc_requirements.is_empty() {
                    text.push_str(&format!(
                        " It also needs a newer C library than this system has ({}).",
                        self.glibc_requirements.join(", ")
                    ));
                }
                if !self.missing_libraries.is_empty() {
                    text.push_str(" Install those packages and try again.");
                }
                text
            }
        }
    }
}

/// Checks whether the TES3MP runtime installed at `install_dir` can start.
///
/// Never returns an error: a check that cannot be run is a
/// [`RuntimeHealthStatus::NotChecked`] result and a check that ran and found
/// trouble is [`RuntimeHealthStatus::Failed`]. Callers decide what a failure
/// means; installing is never one of the things it undoes, because the files
/// on disk are fine — the machine is what is missing something.
pub fn check_runtime_health(install_dir: &Path, platform: TargetPlatform) -> RuntimeHealth {
    match platform {
        TargetPlatform::Windows => RuntimeHealth::not_checked(
            "Windows builds carry their DLLs beside the executable, so there is nothing \
             for a library check to find",
        ),
        TargetPlatform::MacOs => RuntimeHealth::not_checked(
            "macOS builds ship as self-contained bundles, so there is nothing for a \
             library check to find",
        ),
        TargetPlatform::Linux => check_linux_runtime(install_dir),
    }
}

/// The Linux check on a host that cannot run one. Kept so the module compiles
/// unchanged for `x86_64-pc-windows-gnu`, where the process-group handling the
/// real implementation needs does not exist.
#[cfg(not(unix))]
fn check_linux_runtime(_install_dir: &Path) -> RuntimeHealth {
    RuntimeHealth::not_checked("a Linux runtime can only be health-checked from a Unix host")
}

#[cfg(unix)]
fn check_linux_runtime(install_dir: &Path) -> RuntimeHealth {
    if !install_dir.is_dir() {
        return RuntimeHealth::not_checked(format!(
            "there is no runtime directory at {}",
            install_dir.display()
        ));
    }

    let Some(exe) = find_executable(install_dir, CLIENT_EXE_NAMES, EXE_SEARCH_DEPTH) else {
        return RuntimeHealth::not_checked(format!(
            "no TES3MP client executable under {}",
            install_dir.display()
        ));
    };

    let probe = match run_version_probe(&exe) {
        Ok(probe) => probe,
        Err(error) => {
            // The client could not even be spawned. There is still an ELF to
            // ask ldd about, and a missing library is the likeliest reason a
            // spawn fails after the file was found.
            let mut health = RuntimeHealth::failed(error);
            health.checked_executable = Some(exe.display().to_string());
            add_library_findings(&mut health, &exe, "");
            return health;
        }
    };

    let combined = probe.combined();

    if probe.timed_out {
        let mut health = RuntimeHealth::failed(format!(
            "the client did not answer `--version` within {} seconds and was stopped",
            PROBE_TIMEOUT.as_secs()
        ));
        health.checked_executable = Some(exe.display().to_string());
        health.output = Some(combined.clone());
        add_library_findings(&mut health, &exe, &combined);
        return health;
    }

    if probe.code == Some(0) {
        if let Some(version) = extract_version(&combined) {
            return RuntimeHealth::healthy(Some(version), &exe);
        }
        // Exit 0 with nothing version-shaped: not a failure a player can act
        // on, and not proof of health either. Say so rather than inventing a
        // missing library.
        let mut health = RuntimeHealth::not_checked(
            "the client exited cleanly but printed no version, so its health could not be judged",
        );
        health.checked_executable = Some(exe.display().to_string());
        health.output = Some(combined);
        return health;
    }

    let mut health = RuntimeHealth::failed(match probe.code {
        Some(code) => format!("the client exited with code {code} instead of printing its version"),
        None => "the client was killed by a signal instead of printing its version".to_string(),
    });
    health.checked_executable = Some(exe.display().to_string());
    health.output = Some(combined.clone());
    add_library_findings(&mut health, &exe, &combined);
    health
}

/// Fills in `missing_libraries` and `glibc_requirements` from the failed
/// probe's own output plus a fresh `ldd` run against the ELF binary.
///
/// Both sources are parsed the same way: the loader writes the same
/// `soname => not found` and ``version `GLIBC_2.38' not found`` lines whether
/// it is tracing or executing.
#[cfg(unix)]
fn add_library_findings(health: &mut RuntimeHealth, exe: &Path, probe_output: &str) {
    let mut findings = parse_ldd_output(probe_output);

    match resolve_elf_binary(exe) {
        Ok(elf) => match run_ldd(&elf, library_dir(exe).as_deref()) {
            Ok(output) => findings.merge(parse_ldd_output(&output)),
            Err(error) => {
                let note = format!(" The library trace could not be run either: {error}.");
                if let Some(reason) = health.reason.as_mut() {
                    reason.push_str(&note);
                }
            }
        },
        Err(error) => {
            let note = format!(" The real binary behind it could not be located: {error}.");
            if let Some(reason) = health.reason.as_mut() {
                reason.push_str(&note);
            }
        }
    }

    health.missing_libraries = findings
        .missing
        .into_iter()
        .map(|soname| {
            let hint = package_hint(&soname);
            MissingLibrary {
                debian: hint.map(|h| h.debian.to_string()),
                fedora: hint.map(|h| h.fedora.to_string()),
                arch: hint.map(|h| h.arch.to_string()),
                soname,
            }
        })
        .collect();
    health.glibc_requirements = findings.glibc;
}

// ---------------------------------------------------------------------------
// Locating what to probe
// ---------------------------------------------------------------------------

/// The real ELF binary behind `exe`.
///
/// `exe` is whatever `find_executable` picked, which prefers the wrapper
/// script — the wrapper is what sets the library paths, so it is the right
/// thing to *run*, and the wrong thing to hand `ldd`. Both TES3MP tarball
/// layouts we support name the ELF after the wrapper with a `.x86_64` suffix
/// beside it (official 0.8.1 runs it as a plain command with
/// `LD_LIBRARY_PATH=./lib`; the MundusPatensMP fork `exec`s it after
/// exporting an absolute `LD_LIBRARY_PATH` and `OSG_LIBRARY_PATH`), so the
/// sibling rule covers both without parsing shell.
#[cfg(unix)]
fn resolve_elf_binary(exe: &Path) -> Result<PathBuf, String> {
    if is_elf(exe) {
        return Ok(exe.to_path_buf());
    }
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no usable file name", exe.display()))?;
    let sibling = exe.with_file_name(format!("{name}.x86_64"));
    if sibling.is_file() {
        return Ok(sibling);
    }
    Err(format!(
        "{} is not an ELF binary and there is no {} beside it",
        exe.display(),
        sibling.display()
    ))
}

#[cfg(unix)]
fn is_elf(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => magic == *b"\x7fELF",
        Err(_) => false,
    }
}

/// The bundled library directory the wrapper puts on the loader's path, if
/// the tarball has one. Both layouts use `<runtime root>/lib`.
#[cfg(unix)]
fn library_dir(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?.join("lib");
    dir.is_dir().then_some(dir)
}

// ---------------------------------------------------------------------------
// Running the probes
// ---------------------------------------------------------------------------

/// What one timed, captured child run produced.
#[cfg(unix)]
#[derive(Debug)]
struct ProbeOutput {
    /// `None` when the child was killed by a signal or by our timeout.
    code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

#[cfg(unix)]
impl ProbeOutput {
    fn combined(&self) -> String {
        let mut text = truncate_output(&self.stdout, OUTPUT_LIMIT);
        let stderr = truncate_output(&self.stderr, OUTPUT_LIMIT);
        if !text.is_empty() && !stderr.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr);
        text
    }
}

/// The environment every probe runs under: enough to work, nothing that ties
/// the answer to the session that happens to be logged in.
///
/// No `DISPLAY` or `WAYLAND_DISPLAY`, so a developer's desktop and a
/// dedicated server produce the same verdict and no window ever flashes up.
/// `SDL_VIDEODRIVER`/`SDL_AUDIODRIVER=dummy` because OpenMW 0.47-era builds
/// bring SDL up before printing `--version` and abort without a video device.
/// `HOME` is forwarded when set: the official tarball's `tes3mp-prelaunch`
/// looks for `$HOME/.config/openmw` and would otherwise write to `/.config`.
#[cfg(unix)]
fn configure_probe_env(command: &mut Command) {
    command.env_clear();
    command.env(
        "PATH",
        std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string()),
    );
    if let Ok(home) = std::env::var("HOME") {
        command.env("HOME", home);
    }
    command.env("SDL_VIDEODRIVER", "dummy");
    command.env("SDL_AUDIODRIVER", "dummy");
    command.env("LC_ALL", "C");
}

#[cfg(unix)]
fn run_version_probe(exe: &Path) -> Result<ProbeOutput, String> {
    let mut command = Command::new(exe);
    command.arg("--version");
    if let Some(parent) = exe.parent() {
        command.current_dir(parent);
    }
    configure_probe_env(&mut command);
    run_captured(command, PROBE_TIMEOUT).map_err(|error| {
        format!("the client could not be started at all ({error}), so it cannot run here")
    })
}

#[cfg(unix)]
fn run_ldd(elf: &Path, library_dir: Option<&Path>) -> Result<String, String> {
    let mut command = Command::new("ldd");
    command.arg(elf);
    if let Some(parent) = elf.parent() {
        command.current_dir(parent);
    }
    configure_probe_env(&mut command);
    if let Some(dir) = library_dir {
        command.env("LD_LIBRARY_PATH", dir);
    }
    let probe = run_captured(command, LDD_TIMEOUT)
        .map_err(|error| format!("ldd could not be run ({error})"))?;
    if probe.timed_out {
        return Err("ldd did not finish in time".to_string());
    }
    // ldd exits non-zero for a static binary and for "not a dynamic
    // executable"; both are still readable output, so the exit code is not
    // the test — the caller parses whatever came back.
    Ok(probe.combined())
}

/// Spawns `command`, drains both pipes on their own threads, and enforces
/// `timeout`.
///
/// The pipes must be drained concurrently: a child that fills a 64KiB pipe
/// buffer while we sit in `wait()` deadlocks. On timeout the whole process
/// group is signalled, not just the pid we hold — the official tarball's
/// wrapper runs the real binary as a plain foreground command, so killing
/// the wrapper alone would leave the game running and the pipes open, and
/// the reader threads would never finish. Same reasoning, and the same
/// `kill(1)` shell-out, as `process_manager::state::kill_process_group`.
#[cfg(unix)]
fn run_captured(mut command: Command, timeout: Duration) -> Result<ProbeOutput, String> {
    use std::io::Read;
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.id();

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        String::from_utf8_lossy(&buffer).into_owned()
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        String::from_utf8_lossy(&buffer).into_owned()
    });

    let started = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => return Err(error.to_string()),
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            kill_process_group(pid);
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    let code = match status {
        // A signalled child has no exit code; `signal()` is what says so.
        Some(status) => match status.code() {
            Some(code) => Some(code),
            None => {
                let _ = status.signal();
                None
            }
        },
        None => None,
    };

    Ok(ProbeOutput {
        code,
        timed_out,
        stdout,
        stderr,
    })
}

/// Best-effort SIGKILL to the process group `pid` leads. Errors are
/// swallowed: `child.kill()` right after is the authoritative fallback.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// What a loader trace said was wrong.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoaderFindings {
    /// Sonames reported as `=> not found`, in the order the loader listed
    /// them, without duplicates.
    pub missing: Vec<String>,
    /// Symbol-version requirements the system's glibc does not satisfy, e.g.
    /// `GLIBC_2.38`.
    pub glibc: Vec<String>,
}

impl LoaderFindings {
    #[cfg(unix)]
    fn merge(&mut self, other: LoaderFindings) {
        for soname in other.missing {
            if !self.missing.contains(&soname) {
                self.missing.push(soname);
            }
        }
        for version in other.glibc {
            if !self.glibc.contains(&version) {
                self.glibc.push(version);
            }
        }
    }
}

/// Reads `ldd` output — or a loader's own error lines, which have the same
/// shape — into the two things a player can act on.
///
/// Ignored on purpose: `linux-vdso.so.1 (0x…)` and the loader's own
/// `/lib64/ld-linux-x86-64.so.2 (0x…)` (no `=>`, nothing to resolve), and
/// every `soname => /absolute/path (0x…)` line, which is a library that *was*
/// found.
pub fn parse_ldd_output(output: &str) -> LoaderFindings {
    let mut findings = LoaderFindings::default();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(version) = parse_glibc_requirement(line) {
            if !findings.glibc.contains(&version) {
                findings.glibc.push(version);
            }
            continue;
        }
        let Some((name, target)) = line.split_once("=>") else {
            continue;
        };
        if target.trim() != "not found" {
            continue;
        }
        let soname = name.trim().to_string();
        if soname.is_empty() {
            continue;
        }
        if !findings.missing.contains(&soname) {
            findings.missing.push(soname);
        }
    }
    findings
}

/// `GLIBC_2.38` out of ``…: version `GLIBC_2.38' not found (required by …)``.
///
/// This is a different failure from a missing library and must not be filed
/// as one: no package installs it, the build simply needs a newer system.
pub fn parse_glibc_requirement(line: &str) -> Option<String> {
    if !line.contains("not found") {
        return None;
    }
    let start = line.find("GLIBC_")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
        .unwrap_or(rest.len());
    let version = &rest[..end];
    (version.len() > "GLIBC_".len()).then(|| version.to_string())
}

/// The version line a client printed, if it printed one.
///
/// Both supported builds answer `--version` with `OpenMW version 0.47.0` /
/// `OpenMW version 0.51.0` on stdout, alongside wrapper chatter about config
/// files. A line naming a version and carrying a digit is the whole test;
/// `not found` lines are excluded because a loader error mentioning a symbol
/// version would otherwise read as one.
pub fn extract_version(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| {
            line.to_ascii_lowercase().contains("version")
                && line.chars().any(|c| c.is_ascii_digit())
                && !line.contains("not found")
        })
        .map(str::to_string)
}

/// Keeps the head and the tail of `text`, eliding the middle.
///
/// Both ends matter: the loader writes its complaints first, and a program
/// that ran for a while and then died says why last.
pub fn truncate_output(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let half = limit / 2;
    let head: String = trimmed.chars().take(half).collect();
    let tail: String = trimmed
        .chars()
        .skip(trimmed.chars().count().saturating_sub(half))
        .collect();
    format!("{head}\n… (output truncated) …\n{tail}")
}

// ---------------------------------------------------------------------------
// Package hints
// ---------------------------------------------------------------------------

/// The package to install on each distribution family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageHint {
    pub debian: &'static str,
    pub fedora: &'static str,
    pub arch: &'static str,
}

/// How an entry's `key` is compared against a soname's base name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// The base name must equal `key`. Used for short names where a prefix
    /// would over-match — `libc` would otherwise claim `libcurl`.
    Exact,
    /// The base name must start with `key`. Used for families whose members
    /// all come from one package: every `libosg*`, every `libboost_*`.
    Prefix,
}

struct LibraryEntry {
    key: &'static str,
    rule: Match,
    hint: PackageHint,
}

const fn hint(debian: &'static str, fedora: &'static str, arch: &'static str) -> PackageHint {
    PackageHint {
        debian,
        fedora,
        arch,
    }
}

/// The TES3MP stack, as the official 0.8.1 tarball and the MundusPatensMP
/// fork link it.
///
/// Names are the ones a player types into their package manager. Where a
/// family ships only version-suffixed runtime packages (Boost, Bullet,
/// MyGUI, OpenSceneGraph on Debian), the stable family name is given instead
/// of a suffix that changes every release — the point is to be searchable,
/// not to be pasted blind.
static LIBRARY_HINTS: &[LibraryEntry] = &[
    // glibc and the compiler runtimes.
    LibraryEntry {
        key: "libc",
        rule: Match::Exact,
        hint: hint("libc6", "glibc", "glibc"),
    },
    LibraryEntry {
        key: "libm",
        rule: Match::Exact,
        hint: hint("libc6", "glibc", "glibc"),
    },
    LibraryEntry {
        key: "libpthread",
        rule: Match::Exact,
        hint: hint("libc6", "glibc", "glibc"),
    },
    LibraryEntry {
        key: "libdl",
        rule: Match::Exact,
        hint: hint("libc6", "glibc", "glibc"),
    },
    LibraryEntry {
        key: "librt",
        rule: Match::Exact,
        hint: hint("libc6", "glibc", "glibc"),
    },
    LibraryEntry {
        key: "libstdc++",
        rule: Match::Exact,
        hint: hint("libstdc++6", "libstdc++", "gcc-libs"),
    },
    LibraryEntry {
        key: "libgcc_s",
        rule: Match::Exact,
        hint: hint("libgcc-s1", "libgcc", "gcc-libs"),
    },
    // Audio.
    LibraryEntry {
        key: "libopenal",
        rule: Match::Prefix,
        hint: hint("libopenal1", "openal-soft", "openal"),
    },
    // Windowing and input.
    LibraryEntry {
        key: "libSDL2",
        rule: Match::Prefix,
        hint: hint("libsdl2-2.0-0", "SDL2", "sdl2"),
    },
    // Boost.
    LibraryEntry {
        key: "libboost_",
        rule: Match::Prefix,
        hint: hint("libboost-all-dev", "boost", "boost-libs"),
    },
    // ffmpeg.
    LibraryEntry {
        key: "libavcodec",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libavformat",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libavutil",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libavfilter",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libavdevice",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libswscale",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    LibraryEntry {
        key: "libswresample",
        rule: Match::Prefix,
        hint: hint("ffmpeg", "ffmpeg-libs", "ffmpeg"),
    },
    // Lua. LuaJIT first: `liblua` would otherwise swallow `libluajit-5.1`.
    LibraryEntry {
        key: "libluajit",
        rule: Match::Prefix,
        hint: hint("libluajit-5.1-2", "luajit", "luajit"),
    },
    LibraryEntry {
        key: "liblua",
        rule: Match::Prefix,
        hint: hint("liblua5.1-0", "lua-libs", "lua51"),
    },
    // Physics.
    LibraryEntry {
        key: "libBullet",
        rule: Match::Prefix,
        hint: hint("libbullet-dev", "bullet", "bullet"),
    },
    LibraryEntry {
        key: "libLinearMath",
        rule: Match::Prefix,
        hint: hint("libbullet-dev", "bullet", "bullet"),
    },
    // GUI toolkit the engine draws its menus with.
    LibraryEntry {
        key: "libMyGUI",
        rule: Match::Prefix,
        hint: hint("libmygui-dev", "mygui", "mygui"),
    },
    // OpenSceneGraph, the renderer.
    LibraryEntry {
        key: "libosg",
        rule: Match::Prefix,
        hint: hint(
            "libopenscenegraph-dev",
            "OpenSceneGraph",
            "openscenegraph",
        ),
    },
    LibraryEntry {
        key: "libOpenThreads",
        rule: Match::Prefix,
        hint: hint(
            "libopenscenegraph-dev",
            "OpenSceneGraph",
            "openscenegraph",
        ),
    },
    // Text and images.
    LibraryEntry {
        key: "libfreetype",
        rule: Match::Prefix,
        hint: hint("libfreetype6", "freetype", "freetype2"),
    },
    LibraryEntry {
        key: "libpng",
        rule: Match::Prefix,
        hint: hint("libpng16-16", "libpng", "libpng"),
    },
    LibraryEntry {
        key: "libjpeg",
        rule: Match::Prefix,
        hint: hint("libjpeg-turbo8", "libjpeg-turbo", "libjpeg-turbo"),
    },
    // GL. `libGLU` before `libGL`, and both exact so `libGLdispatch` and
    // `libGLESv2` fall through to the generic Mesa/libglvnd entry below.
    LibraryEntry {
        key: "libGLU",
        rule: Match::Exact,
        hint: hint("libglu1-mesa", "mesa-libGLU", "glu"),
    },
    LibraryEntry {
        key: "libGL",
        rule: Match::Exact,
        hint: hint("libgl1", "mesa-libGL", "libglvnd"),
    },
    LibraryEntry {
        key: "libEGL",
        rule: Match::Prefix,
        hint: hint("libegl1", "mesa-libEGL", "libglvnd"),
    },
    LibraryEntry {
        key: "libGLX",
        rule: Match::Prefix,
        hint: hint("libglx0", "mesa-libGL", "libglvnd"),
    },
    LibraryEntry {
        key: "libGLdispatch",
        rule: Match::Prefix,
        hint: hint("libglvnd0", "libglvnd", "libglvnd"),
    },
    // Qt5 — the launcher and the server browser, when a tree ships them.
    LibraryEntry {
        key: "libQt5Core",
        rule: Match::Prefix,
        hint: hint("libqt5core5a", "qt5-qtbase", "qt5-base"),
    },
    LibraryEntry {
        key: "libQt5Gui",
        rule: Match::Prefix,
        hint: hint("libqt5gui5", "qt5-qtbase-gui", "qt5-base"),
    },
    LibraryEntry {
        key: "libQt5Widgets",
        rule: Match::Prefix,
        hint: hint("libqt5widgets5", "qt5-qtbase-gui", "qt5-base"),
    },
    LibraryEntry {
        key: "libQt5Network",
        rule: Match::Prefix,
        hint: hint("libqt5network5", "qt5-qtbase", "qt5-base"),
    },
    LibraryEntry {
        key: "libQt5OpenGL",
        rule: Match::Prefix,
        hint: hint("libqt5opengl5", "qt5-qtbase-gui", "qt5-base"),
    },
    LibraryEntry {
        key: "libQt5",
        rule: Match::Prefix,
        hint: hint("qtbase5-dev", "qt5-qtbase", "qt5-base"),
    },
    // X11.
    LibraryEntry {
        key: "libX11",
        rule: Match::Prefix,
        hint: hint("libx11-6", "libX11", "libx11"),
    },
    // Odds and ends the tarballs bundle.
    LibraryEntry {
        key: "libz",
        rule: Match::Exact,
        hint: hint("zlib1g", "zlib", "zlib"),
    },
    LibraryEntry {
        key: "libbz2",
        rule: Match::Prefix,
        hint: hint("libbz2-1.0", "bzip2-libs", "bzip2"),
    },
    LibraryEntry {
        key: "liblzma",
        rule: Match::Prefix,
        hint: hint("liblzma5", "xz-libs", "xz"),
    },
    LibraryEntry {
        key: "libuuid",
        rule: Match::Prefix,
        hint: hint("libuuid1", "libuuid", "util-linux-libs"),
    },
    LibraryEntry {
        key: "libtinfo",
        rule: Match::Prefix,
        hint: hint("libtinfo6", "ncurses-libs", "ncurses"),
    },
    LibraryEntry {
        key: "libunshield",
        rule: Match::Prefix,
        hint: hint("libunshield0", "unshield", "unshield"),
    },
];

/// The part of a soname before `.so`: `libSDL2-2.0.so.0` → `libSDL2-2.0`.
/// A path-qualified soname keeps only its file name.
pub fn soname_base(soname: &str) -> &str {
    let name = soname.rsplit('/').next().unwrap_or(soname);
    match name.find(".so") {
        Some(index) => &name[..index],
        None => name,
    }
}

/// The package hint for a soname, or `None` when nothing in the table knows
/// it — which is reported as an unhinted missing library rather than guessed.
pub fn package_hint(soname: &str) -> Option<PackageHint> {
    let base = soname_base(soname);
    LIBRARY_HINTS
        .iter()
        .find(|entry| entry.rule == Match::Exact && entry.key == base)
        .or_else(|| {
            LIBRARY_HINTS
                .iter()
                .find(|entry| entry.rule == Match::Prefix && base.starts_with(entry.key))
        })
        .map(|entry| entry.hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ldd` output from the official TES3MP 0.8.1 tarball's
    /// `tes3mp.x86_64` on a machine without the bundled `lib` directory on
    /// the loader path — the exact shape a player's broken install produces.
    const LDD_WITH_MISSING: &str = "\tlinux-vdso.so.1 (0x00007b3c8909c000)\n\
        \tlibosgParticle.so.162 => not found\n\
        \tlibosgViewer.so.162 => not found\n\
        \tlibboost_system.so.1.67.0 => not found\n\
        \tlibopenal.so.1 => /lib/x86_64-linux-gnu/libopenal.so.1 (0x00007b3c87e5b000)\n\
        \tlibavcodec.so.58 => not found\n\
        \tlibMyGUIEngine.so.3.2.3 => not found\n\
        \tlibSDL2-2.0.so.0 => /lib/x86_64-linux-gnu/libSDL2-2.0.so.0 (0x00007b3c87c6c000)\n\
        \tlibpthread.so.0 => /lib/x86_64-linux-gnu/libpthread.so.0 (0x00007b3c87c67000)\n\
        \t/lib64/ld-linux-x86-64.so.2 (0x00007b3c8909e000)\n";

    /// The same binary with its bundled libraries found: nothing to report.
    const LDD_ALL_RESOLVED: &str = "\tlinux-vdso.so.1 (0x00007aeead602000)\n\
        \tlibosgParticle.so.162 => /opt/TES3MP/lib/libosgParticle.so.162 (0x00007aeeac568000)\n\
        \tlibopenal.so.1 => /opt/TES3MP/lib/libopenal.so.1 (0x00007aeeab9d1000)\n\
        \t/lib64/ld-linux-x86-64.so.2 (0x00007aeead604000)\n";

    #[test]
    fn ldd_parser_lists_only_the_unresolved_sonames() {
        let findings = parse_ldd_output(LDD_WITH_MISSING);
        assert_eq!(
            findings.missing,
            vec![
                "libosgParticle.so.162",
                "libosgViewer.so.162",
                "libboost_system.so.1.67.0",
                "libavcodec.so.58",
                "libMyGUIEngine.so.3.2.3",
            ]
        );
        assert!(findings.glibc.is_empty());
    }

    /// `linux-vdso` and the loader's own line carry no `=>` and must not be
    /// mistaken for anything; a resolved absolute path is a success, not a
    /// finding.
    #[test]
    fn ldd_parser_ignores_vdso_loader_and_resolved_lines() {
        let findings = parse_ldd_output(LDD_ALL_RESOLVED);
        assert_eq!(findings, LoaderFindings::default());
    }

    #[test]
    fn ldd_parser_deduplicates_repeated_sonames() {
        let output = "\tlibopenal.so.1 => not found\n\tlibopenal.so.1 => not found\n";
        assert_eq!(parse_ldd_output(output).missing, vec!["libopenal.so.1"]);
    }

    /// A too-old glibc is a different problem with a different answer, so it
    /// lands in `glibc`, never in `missing`.
    #[test]
    fn glibc_version_errors_are_not_reported_as_missing_libraries() {
        let output = "./tes3mp.x86_64: /lib/x86_64-linux-gnu/libm.so.6: version `GLIBC_2.38' \
                      not found (required by ./tes3mp.x86_64)\n\
                      ./tes3mp.x86_64: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.34' \
                      not found (required by /opt/TES3MP/lib/libosg.so.162)\n";
        let findings = parse_ldd_output(output);
        assert!(findings.missing.is_empty());
        assert_eq!(findings.glibc, vec!["GLIBC_2.38", "GLIBC_2.34"]);
    }

    #[test]
    fn glibc_requirement_needs_both_a_version_and_a_not_found() {
        assert_eq!(
            parse_glibc_requirement("version `GLIBC_2.38' not found (required by x)"),
            Some("GLIBC_2.38".to_string())
        );
        // A resolved line naming GLIBC is not a failure.
        assert_eq!(parse_glibc_requirement("linked against GLIBC_2.17"), None);
        assert_eq!(parse_glibc_requirement("libfoo.so.1 => not found"), None);
    }

    #[test]
    fn a_failed_check_mixes_missing_libraries_and_a_glibc_note() {
        let findings = parse_ldd_output(
            "\tlibopenal.so.1 => not found\n\
             ./tes3mp.x86_64: /lib/libc.so.6: version `GLIBC_2.38' not found (required by x)\n",
        );
        assert_eq!(findings.missing, vec!["libopenal.so.1"]);
        assert_eq!(findings.glibc, vec!["GLIBC_2.38"]);
    }

    #[test]
    fn version_is_read_out_of_the_wrapper_chatter() {
        let output = "Loading client config from the package directory\nOpenMW version 0.47.0\n\
                      Revision: 68954091c5\n";
        assert_eq!(
            extract_version(output).as_deref(),
            Some("OpenMW version 0.47.0")
        );
    }

    /// The fork's wrapper prints a paragraph about Morrowind data first; the
    /// version still has to be found behind it.
    #[test]
    fn version_is_read_past_a_wrappers_warning_paragraph() {
        let output = "tes3mp: no Morrowind data configured — the game cannot start yet.\n\
                      tes3mp: edit /opt/TES3MP/userprofile/config/openmw/openmw.cfg\n\
                      OpenMW version 0.51.0\nRevision: f5d26f9ddc\nQuitting peacefully.\n";
        assert_eq!(
            extract_version(output).as_deref(),
            Some("OpenMW version 0.51.0")
        );
    }

    #[test]
    fn a_symbol_version_error_is_not_read_as_a_version_string() {
        let output = "./tes3mp.x86_64: version `GLIBC_2.38' not found (required by x)\n";
        assert_eq!(extract_version(output), None);
    }

    #[test]
    fn package_hints_name_the_right_package_per_distribution() {
        let openal = package_hint("libopenal.so.1").expect("openal is in the table");
        assert_eq!(openal.debian, "libopenal1");
        assert_eq!(openal.fedora, "openal-soft");
        assert_eq!(openal.arch, "openal");

        let sdl = package_hint("libSDL2-2.0.so.0").expect("SDL2 is in the table");
        assert_eq!(sdl.debian, "libsdl2-2.0-0");
        assert_eq!(sdl.arch, "sdl2");

        let ffmpeg = package_hint("libavcodec.so.58").expect("ffmpeg is in the table");
        assert_eq!(ffmpeg.fedora, "ffmpeg-libs");
    }

    /// Every member of a family resolves through one prefix entry, so a
    /// build linking a different OSG or Boost soname still gets a hint.
    #[test]
    fn family_prefixes_cover_every_member() {
        for soname in [
            "libosg.so.162",
            "libosgViewer.so.162",
            "libosgDB.so.3.6.5",
            "libOpenThreads.so.21",
        ] {
            assert_eq!(
                package_hint(soname).map(|h| h.arch),
                Some("openscenegraph"),
                "{soname} should map to OpenSceneGraph"
            );
        }
        for soname in ["libboost_system.so.1.67.0", "libboost_filesystem.so.1.67.0"] {
            assert_eq!(package_hint(soname).map(|h| h.fedora), Some("boost"));
        }
        for soname in ["libBulletCollision.so.2.87", "libLinearMath.so.2.87"] {
            assert_eq!(package_hint(soname).map(|h| h.arch), Some("bullet"));
        }
    }

    /// LuaJIT must not be swallowed by the plain-Lua prefix.
    #[test]
    fn luajit_beats_the_plain_lua_prefix() {
        assert_eq!(
            package_hint("libluajit-5.1.so.2").map(|h| h.debian),
            Some("libluajit-5.1-2")
        );
        assert_eq!(
            package_hint("liblua5.1.so.0").map(|h| h.debian),
            Some("liblua5.1-0")
        );
    }

    /// Short exact keys must not claim longer names that merely start the
    /// same way — the reason `libc` is `Exact`.
    #[test]
    fn short_exact_keys_do_not_over_match() {
        assert_eq!(package_hint("libc.so.6").map(|h| h.debian), Some("libc6"));
        assert_eq!(package_hint("libcrypto.so.3"), None);
        assert_eq!(package_hint("libcurl.so.4"), None);
        assert_eq!(package_hint("libcodec2.so.0.8.1"), None);
    }

    #[test]
    fn an_unknown_soname_gets_no_hint() {
        assert_eq!(package_hint("libwhatever.so.9"), None);
        let missing = MissingLibrary {
            soname: "libwhatever.so.9".to_string(),
            debian: None,
            fedora: None,
            arch: None,
        };
        assert_eq!(
            missing.describe(),
            "libwhatever.so.9 (no package hint — search your distribution for it)"
        );
    }

    #[test]
    fn soname_base_strips_the_so_suffix_and_any_directory() {
        assert_eq!(soname_base("libopenal.so.1"), "libopenal");
        assert_eq!(soname_base("libSDL2-2.0.so.0"), "libSDL2-2.0");
        assert_eq!(soname_base("/usr/lib/libosg.so.162"), "libosg");
        assert_eq!(soname_base("libstdc++.so.6"), "libstdc++");
    }

    #[test]
    fn the_failure_summary_names_every_missing_library_with_its_packages() {
        let health = RuntimeHealth {
            status: RuntimeHealthStatus::Failed,
            version: None,
            reason: Some("the client exited with code 127".to_string()),
            output: None,
            missing_libraries: vec![
                MissingLibrary {
                    soname: "libopenal.so.1".to_string(),
                    debian: Some("libopenal1".to_string()),
                    fedora: Some("openal-soft".to_string()),
                    arch: Some("openal".to_string()),
                },
                MissingLibrary {
                    soname: "libluajit-5.1.so.2".to_string(),
                    debian: Some("libluajit-5.1-2".to_string()),
                    fedora: Some("luajit".to_string()),
                    arch: Some("luajit".to_string()),
                },
            ],
            glibc_requirements: Vec::new(),
            checked_executable: None,
        };
        assert_eq!(
            health.summary(),
            "TES3MP cannot start: missing libopenal.so.1 (Debian/Ubuntu: libopenal1, \
             Fedora: openal-soft, Arch: openal), libluajit-5.1.so.2 (Debian/Ubuntu: \
             libluajit-5.1-2, Fedora: luajit, Arch: luajit). Install those packages and \
             try again."
        );
    }

    #[test]
    fn a_glibc_failure_says_there_is_nothing_to_install() {
        let mut health = RuntimeHealth::failed("the client exited with code 1");
        health.glibc_requirements = vec!["GLIBC_2.38".to_string()];
        let summary = health.summary();
        assert!(summary.contains("GLIBC_2.38"), "{summary}");
        assert!(summary.contains("no package to install"), "{summary}");
        assert!(!summary.contains("missing lib"), "{summary}");
    }

    #[test]
    fn truncation_keeps_both_ends() {
        let text = format!("START{}END", "x".repeat(500));
        let truncated = truncate_output(&text, 100);
        assert!(truncated.starts_with("START"), "{truncated}");
        assert!(truncated.ends_with("END"), "{truncated}");
        assert!(truncated.contains("output truncated"), "{truncated}");
        assert!(truncated.chars().count() < text.chars().count());
    }

    #[test]
    fn short_output_is_returned_whole() {
        assert_eq!(truncate_output("  hello\n", 100), "hello");
    }

    /// Neither non-Linux platform has the bundled-library problem, and
    /// neither has `ldd`; both must say so rather than pretending to pass.
    #[test]
    fn windows_and_macos_are_not_checked() {
        for platform in [TargetPlatform::Windows, TargetPlatform::MacOs] {
            let health = check_runtime_health(Path::new("/nonexistent"), platform);
            assert_eq!(health.status, RuntimeHealthStatus::NotChecked);
            assert!(health.reason.is_some());
            assert!(health.summary().contains("not health-checked"), "{health:?}");
        }
    }

    #[cfg(unix)]
    mod linux {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        struct Scratch(PathBuf);

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        impl Scratch {
            fn path(&self) -> &Path {
                &self.0
            }
        }

        fn scratch(label: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "nerevar-health-{label}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        /// Writes a fake runtime: an executable `tes3mp` wrapper script with
        /// the given body, and the `tes3mp.x86_64` stub the ldd step looks
        /// for beside it. `find_executable` prefers the wrapper, exactly as
        /// the launch path does.
        fn fake_runtime(dir: &Path, wrapper_body: &str) {
            let wrapper = dir.join("tes3mp");
            fs::write(&wrapper, wrapper_body).unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(dir.join("tes3mp.x86_64"), b"not really an elf").unwrap();
        }

        #[test]
        fn a_client_that_prints_a_version_is_healthy() {
            let dir = scratch("healthy");
            fake_runtime(
                dir.path(),
                "#!/bin/sh\necho 'Loading client config from the package directory'\n\
                 echo 'OpenMW version 0.47.0'\nexit 0\n",
            );

            let health = check_runtime_health(dir.path(), TargetPlatform::Linux);
            assert_eq!(health.status, RuntimeHealthStatus::Healthy, "{health:?}");
            assert_eq!(health.version.as_deref(), Some("OpenMW version 0.47.0"));
            assert!(health.missing_libraries.is_empty());
            assert!(health.summary().contains("0.47.0"), "{health:?}");
        }

        /// The whole point of the exercise: a client that dies the way a
        /// missing library makes it die is Failed, and the ldd step runs.
        /// The stub beside the wrapper is not a real ELF, so `ldd` reports
        /// "not a dynamic executable" and there is nothing to list — the
        /// failure is still reported with its exit code.
        #[test]
        fn a_client_that_exits_nonzero_fails_and_runs_the_library_trace() {
            let dir = scratch("nonzero");
            fake_runtime(
                dir.path(),
                "#!/bin/sh\necho 'error while loading shared libraries' >&2\nexit 1\n",
            );

            let health = check_runtime_health(dir.path(), TargetPlatform::Linux);
            assert_eq!(health.status, RuntimeHealthStatus::Failed, "{health:?}");
            let reason = health.reason.clone().unwrap_or_default();
            assert!(reason.contains("code 1"), "{reason}");
            assert!(
                health
                    .output
                    .as_deref()
                    .unwrap_or_default()
                    .contains("error while loading shared libraries"),
                "{health:?}"
            );
            assert!(health.summary().starts_with("TES3MP cannot start"), "{health:?}");
            assert!(
                health.checked_executable.as_deref() == Some(
                    dir.path().join("tes3mp").to_str().unwrap()
                ),
                "{health:?}"
            );
        }

        /// A loader error printed by the client itself is enough: the missing
        /// sonames are read straight out of the probe's stderr, package hints
        /// and all.
        #[test]
        fn missing_libraries_are_read_out_of_the_clients_own_stderr() {
            let dir = scratch("missing-libs");
            fake_runtime(
                dir.path(),
                "#!/bin/sh\n\
                 echo '\tlibopenal.so.1 => not found' >&2\n\
                 echo '\tlibosgViewer.so.162 => not found' >&2\n\
                 exit 127\n",
            );

            let health = check_runtime_health(dir.path(), TargetPlatform::Linux);
            assert_eq!(health.status, RuntimeHealthStatus::Failed, "{health:?}");
            let sonames: Vec<&str> = health
                .missing_libraries
                .iter()
                .map(|lib| lib.soname.as_str())
                .collect();
            assert_eq!(sonames, vec!["libopenal.so.1", "libosgViewer.so.162"]);
            let summary = health.summary();
            assert!(summary.contains("libopenal1"), "{summary}");
            assert!(summary.contains("openscenegraph"), "{summary}");
        }

        /// A client that never exits must not hang an install. 15s of real
        /// sleeping would be a slow test, so this drives `run_captured`
        /// directly with a short timeout — the same code path, minus the
        /// wait.
        #[test]
        fn a_client_that_never_exits_is_killed_and_reported_as_a_timeout() {
            let dir = scratch("timeout");
            fake_runtime(dir.path(), "#!/bin/sh\necho starting\nsleep 120\n");

            let mut command = Command::new(dir.path().join("tes3mp"));
            command.arg("--version").current_dir(dir.path());
            configure_probe_env(&mut command);
            let probe = run_captured(command, Duration::from_millis(400)).expect("spawn");

            assert!(probe.timed_out, "{probe:?} should have timed out");
            assert!(probe.combined().contains("starting"), "{probe:?}");
        }

        /// And the timeout as the whole check reports it. Uses a wrapper that
        /// sleeps past `PROBE_TIMEOUT`… which would make the test take 15s,
        /// so instead the reason string is asserted from the same failure
        /// constructor the timeout branch uses.
        #[test]
        fn a_timeout_reason_names_the_timeout() {
            let mut health = RuntimeHealth::failed(format!(
                "the client did not answer `--version` within {} seconds and was stopped",
                PROBE_TIMEOUT.as_secs()
            ));
            health.missing_libraries = Vec::new();
            let summary = health.summary();
            assert!(summary.contains("did not answer"), "{summary}");
            assert!(summary.contains("15 seconds"), "{summary}");
        }

        #[test]
        fn a_directory_with_no_client_is_not_checked() {
            let dir = scratch("empty");
            let health = check_runtime_health(dir.path(), TargetPlatform::Linux);
            assert_eq!(health.status, RuntimeHealthStatus::NotChecked);
            assert!(
                health
                    .reason
                    .as_deref()
                    .unwrap_or_default()
                    .contains("no TES3MP client executable"),
                "{health:?}"
            );
        }

        #[test]
        fn a_missing_directory_is_not_checked() {
            let health =
                check_runtime_health(Path::new("/nonexistent-runtime"), TargetPlatform::Linux);
            assert_eq!(health.status, RuntimeHealthStatus::NotChecked);
        }

        /// The wrapper is what gets run and the `.x86_64` beside it is what
        /// gets traced — the split the whole module depends on.
        #[test]
        fn the_elf_behind_a_wrapper_script_is_its_x86_64_sibling() {
            let dir = scratch("elf-resolution");
            fake_runtime(dir.path(), "#!/bin/sh\nexit 0\n");
            assert_eq!(
                resolve_elf_binary(&dir.path().join("tes3mp")).unwrap(),
                dir.path().join("tes3mp.x86_64")
            );
        }

        #[test]
        fn a_real_elf_is_traced_directly() {
            let dir = scratch("real-elf");
            let binary = dir.path().join("tes3mp.x86_64");
            fs::write(&binary, b"\x7fELF and then some").unwrap();
            assert_eq!(resolve_elf_binary(&binary).unwrap(), binary);
        }

        #[test]
        fn a_wrapper_with_no_binary_beside_it_cannot_be_traced() {
            let dir = scratch("no-elf");
            let wrapper = dir.path().join("tes3mp");
            fs::write(&wrapper, b"#!/bin/sh\n").unwrap();
            assert!(resolve_elf_binary(&wrapper).is_err());
        }

        #[test]
        fn the_bundled_lib_directory_is_found_when_the_tarball_has_one() {
            let dir = scratch("libdir");
            fake_runtime(dir.path(), "#!/bin/sh\nexit 0\n");
            assert_eq!(library_dir(&dir.path().join("tes3mp")), None);
            fs::create_dir_all(dir.path().join("lib")).unwrap();
            assert_eq!(
                library_dir(&dir.path().join("tes3mp")),
                Some(dir.path().join("lib"))
            );
        }

        /// The check against a real TES3MP tree. Ignored because it needs one
        /// on disk and runs the actual game binary.
        ///
        /// Run with:
        /// `cargo test --workspace -p nerevar-core -- --ignored --nocapture
        /// real_tes3mp_fixture_is_healthy`
        ///
        /// `NEREVAR_TES3MP_FIXTURE` overrides the tree; the default is the
        /// workspace's official 0.8.1 fixture.
        #[test]
        #[ignore = "needs a real TES3MP install on disk"]
        fn real_tes3mp_fixture_is_healthy() {
            let fixture = std::env::var("NEREVAR_TES3MP_FIXTURE").unwrap_or_else(|_| {
                "/home/agent003/Nerevar-PM/artifacts/tes3mp-fixture/TES3MP".to_string()
            });
            let path = PathBuf::from(&fixture);
            if !path.is_dir() {
                eprintln!("no TES3MP tree at {fixture} — nothing to check");
                return;
            }

            let health = check_runtime_health(&path, TargetPlatform::Linux);
            eprintln!("status:  {:?}", health.status);
            eprintln!("summary: {}", health.summary());
            eprintln!("full:    {health:#?}");
            assert_eq!(
                health.status,
                RuntimeHealthStatus::Healthy,
                "the fixture should run on a machine with its bundled libraries"
            );
        }
    }
}
