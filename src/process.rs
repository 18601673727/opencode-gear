//! Centralized external process execution.
//!
//! This module is the only place that builds a [`std::process::Command`], so
//! every child process the tool ever starts goes through one auditable path.
//!
//! [`ProcessRunner`] replaces the current process with OpenCode.
//! [`ProcessHost`] is the runtime-facing side (PATH lookup, `--version`
//! probing and OpenCode's own `upgrade`); tests inject a fake host instead of
//! spawning anything.

use crate::error::{GearError, Result};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The `opencode` binary and how to run it.
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    program: OsString,
}

impl ProcessRunner {
    pub fn new(program: OsString) -> Self {
        Self { program }
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// Replace the current process with the configured program.
    ///
    /// `OPENCODE_CONFIG_CONTENT` carries the generated config;
    /// `OPENCODE_CONFIG` is removed so a stale file path cannot override it.
    /// On Unix this `exec`s so signals and exit codes behave exactly like the
    /// historical shell wrapper.
    pub fn exec(&self, args: &[OsString], cwd: &Path, config_content: &str) -> Result<()> {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .current_dir(cwd)
            .env("OPENCODE_CONFIG_CONTENT", config_content)
            .env_remove("OPENCODE_CONFIG");

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = command.exec();
            Err(GearError::io(
                format!("cannot run {}", self.program.to_string_lossy()),
                error,
            ))
        }

        #[cfg(not(unix))]
        {
            let status = command.status().map_err(|error| {
                GearError::io(
                    format!("cannot run {}", self.program.to_string_lossy()),
                    error,
                )
            })?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
}

/// Process discovery, version probing and OpenCode's own upgrade command.
pub trait ProcessHost: Send + Sync {
    /// Resolve a bare program name against `PATH`.
    fn find_in_path(&self, program: &str) -> Option<PathBuf>;

    /// Run `<program> --version` and return the trimmed output.
    fn version(&self, program: &Path) -> Result<String>;

    /// Run OpenCode's own `<program> upgrade` and return its output.
    ///
    /// This is the only upgrade path for an existing system runtime; it must
    /// never be replaced by a managed download.
    fn upgrade(&self, program: &Path) -> Result<String>;
}

/// The real host, backed by `PATH` and `std::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProcessHost;

impl ProcessHost for SystemProcessHost {
    fn find_in_path(&self, program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&path) {
            if directory.as_os_str().is_empty() {
                continue;
            }
            let candidate = directory.join(program);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            #[cfg(windows)]
            {
                for extension in ["exe", "cmd", "bat"] {
                    let candidate = directory.join(format!("{program}.{extension}"));
                    if is_executable(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    fn version(&self, program: &Path) -> Result<String> {
        let output = Command::new(program)
            .arg("--version")
            .output()
            .map_err(|error| {
                GearError::io(format!("cannot run {} --version", program.display()), error)
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                stderr
            };
            return Err(GearError::config(format!(
                "{} --version failed: {detail}",
                program.display()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !stdout.is_empty() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stderr.is_empty() {
            return Ok(stderr);
        }
        Err(GearError::config(format!(
            "{} --version produced no output",
            program.display()
        )))
    }

    fn upgrade(&self, program: &Path) -> Result<String> {
        let output = Command::new(program)
            .arg("upgrade")
            .output()
            .map_err(|error| {
                GearError::io(format!("cannot run {} upgrade", program.display()), error)
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit status {}", output.status)
            };
            return Err(GearError::config(format!(
                "{} upgrade failed: {detail}",
                program.display()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Ok(if !stdout.is_empty() { stdout } else { stderr })
    }
}

/// The outcome configured for a fake `upgrade` call.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum FakeUpgrade {
    /// Succeed, optionally changing the version the host reports afterwards.
    Success(Option<String>),
    /// Fail with a message.
    Failure(String),
}

/// A fake host for tests: a fixed `PATH` map, canned version strings, canned
/// `upgrade` outcomes and call counters.
#[doc(hidden)]
#[derive(Debug, Default, Clone)]
pub struct FakeProcessHost {
    programs: HashMap<String, PathBuf>,
    versions: Arc<Mutex<HashMap<PathBuf, String>>>,
    default_version: Option<String>,
    upgrades: Arc<Mutex<HashMap<PathBuf, FakeUpgrade>>>,
    upgrade_calls: Arc<Mutex<Vec<PathBuf>>>,
    version_calls: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeProcessHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_program(mut self, name: &str, path: impl Into<PathBuf>) -> Self {
        self.programs.insert(name.to_string(), path.into());
        self
    }

    pub fn with_version(self, path: impl Into<PathBuf>, version: &str) -> Self {
        self.versions
            .lock()
            .expect("fake versions")
            .insert(path.into(), version.to_string());
        self
    }

    /// Report this version for any path without a specific version.
    pub fn with_default_version(mut self, version: &str) -> Self {
        self.default_version = Some(version.to_string());
        self
    }

    /// Configure `<path> upgrade` to succeed, optionally moving to a new version.
    pub fn with_upgrade_success(self, path: impl Into<PathBuf>, new_version: Option<&str>) -> Self {
        self.upgrades.lock().expect("fake upgrades").insert(
            path.into(),
            FakeUpgrade::Success(new_version.map(str::to_string)),
        );
        self
    }

    /// Configure `<path> upgrade` to fail.
    pub fn with_upgrade_failure(self, path: impl Into<PathBuf>, message: &str) -> Self {
        self.upgrades
            .lock()
            .expect("fake upgrades")
            .insert(path.into(), FakeUpgrade::Failure(message.to_string()));
        self
    }

    /// Paths for which `upgrade` was invoked, in order.
    pub fn upgrade_calls(&self) -> Vec<PathBuf> {
        self.upgrade_calls.lock().expect("upgrade calls").clone()
    }

    /// Paths for which `--version` was invoked, in order.
    pub fn version_calls(&self) -> Vec<PathBuf> {
        self.version_calls.lock().expect("version calls").clone()
    }
}

impl ProcessHost for FakeProcessHost {
    fn find_in_path(&self, program: &str) -> Option<PathBuf> {
        self.programs.get(program).cloned()
    }

    fn version(&self, program: &Path) -> Result<String> {
        self.version_calls
            .lock()
            .expect("version calls")
            .push(program.to_path_buf());
        let versions = self.versions.lock().expect("fake versions");
        if let Some(version) = versions.get(program) {
            return Ok(version.clone());
        }
        drop(versions);
        self.default_version.clone().ok_or_else(|| {
            GearError::config(format!(
                "{} --version produced no output",
                program.display()
            ))
        })
    }

    fn upgrade(&self, program: &Path) -> Result<String> {
        self.upgrade_calls
            .lock()
            .expect("upgrade calls")
            .push(program.to_path_buf());
        let outcome = self
            .upgrades
            .lock()
            .expect("fake upgrades")
            .get(program)
            .cloned();
        match outcome {
            Some(FakeUpgrade::Success(new_version)) => {
                if let Some(new_version) = new_version {
                    self.versions
                        .lock()
                        .expect("fake versions")
                        .insert(program.to_path_buf(), new_version);
                }
                Ok("upgraded".to_string())
            }
            Some(FakeUpgrade::Failure(message)) => Err(GearError::config(message)),
            None => Err(GearError::config(format!(
                "{} upgrade is not configured in this fake",
                program.display()
            ))),
        }
    }
}

/// A captured `git` invocation.
///
/// A non-zero exit status is *data*, not an error: "this is not a git
/// repository" is a normal answer the context engine must handle gracefully.
/// Only a failure to spawn `git` at all is an [`Err`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// Set when stdout or stderr was capped by [`GitHost::run_bounded`].
    pub truncated: bool,
}

/// The only place the context engine reaches Git. Keeping it here preserves the
/// "every child process goes through one auditable module" invariant.
pub trait GitHost: Send + Sync {
    /// Run `git` with `args` in `cwd` and capture its output.
    fn run(&self, args: &[&str], cwd: &Path) -> Result<GitOutput>;

    /// Run `git` and capture at most `max_bytes` of stdout, killing the child if
    /// it keeps producing output. The default implementation truncates the
    /// result of [`GitHost::run`] on a UTF-8 boundary, which is deterministic
    /// and sufficient for fakes; real hosts should override it.
    fn run_bounded(&self, args: &[&str], cwd: &Path, max_bytes: usize) -> Result<GitOutput> {
        let mut output = self.run(args, cwd)?;
        if output.stdout.len() > max_bytes {
            let mut end = max_bytes;
            while end > 0 && !output.stdout.is_char_boundary(end) {
                end -= 1;
            }
            output.stdout.truncate(end);
            output.truncated = true;
        }
        Ok(output)
    }
}

/// Read at most `max` bytes, stopping (and signalling truncation) as soon as
/// the cap is reached. Used for stdout, where the caller kills the child.
fn read_bounded<R: std::io::Read>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = max.saturating_sub(out.len());
                if remaining == 0 {
                    truncated = true;
                    break;
                }
                let take = read.min(remaining);
                out.extend_from_slice(&buffer[..take]);
                if take < read {
                    truncated = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (out, truncated)
}

/// Drain a stream to EOF while retaining at most `max` bytes. Used for stderr,
/// so a full pipe can never deadlock the child.
fn read_drain_bounded<R: std::io::Read>(mut reader: R, max: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = max.saturating_sub(out.len());
                if remaining == 0 {
                    truncated = true;
                    continue;
                }
                let take = read.min(remaining);
                out.extend_from_slice(&buffer[..take]);
                if take < read {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (out, truncated)
}

/// The real Git host, backed by `std::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGitHost;

impl GitHost for SystemGitHost {
    fn run(&self, args: &[&str], cwd: &Path) -> Result<GitOutput> {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|error| GearError::io("cannot run git", error))?;
        Ok(GitOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            truncated: false,
        })
    }

    fn run_bounded(&self, args: &[&str], cwd: &Path, max_bytes: usize) -> Result<GitOutput> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| GearError::io("cannot run git", error))?;

        // Drain stderr concurrently, retaining a bounded prefix.
        let stderr_handle = child
            .stderr
            .take()
            .map(|stderr| std::thread::spawn(move || read_drain_bounded(stderr, max_bytes)));

        let (stdout_bytes, stdout_truncated) = match child.stdout.take() {
            Some(stdout) => read_bounded(stdout, max_bytes),
            None => (Vec::new(), false),
        };
        // Stop a runaway producer before waiting on it.
        if stdout_truncated {
            let _ = child.kill();
        }
        let status = match child.wait() {
            Ok(status) => status,
            Err(error) => {
                if let Some(handle) = stderr_handle {
                    let _ = handle.join();
                }
                return Err(GearError::io("cannot wait for git", error));
            }
        };
        let (stderr_bytes, stderr_truncated) = match stderr_handle {
            Some(handle) => handle.join().unwrap_or_default(),
            None => (Vec::new(), false),
        };
        Ok(GitOutput {
            success: status.success(),
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}

/// One exact-argument fake git response.
#[doc(hidden)]
pub type FakeGitResponse = (Vec<String>, GitOutput);

/// A fake Git host for tests: exact-argument responses, with a default
/// "not a repository" answer for unmatched calls.
#[doc(hidden)]
#[derive(Debug, Default, Clone)]
pub struct FakeGitHost {
    responses: Arc<Mutex<Vec<FakeGitResponse>>>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    default: Option<GitOutput>,
}

impl FakeGitHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the output for an exact argument vector (ignoring `cwd`).
    pub fn with_response(self, args: &[&str], output: GitOutput) -> Self {
        self.responses
            .lock()
            .expect("fake git responses")
            .push((args.iter().map(|arg| arg.to_string()).collect(), output));
        self
    }

    /// Convenience: a successful response with the given stdout.
    pub fn with_stdout(self, args: &[&str], stdout: &str) -> Self {
        self.with_response(
            args,
            GitOutput {
                success: true,
                stdout: stdout.to_string(),
                stderr: String::new(),
                truncated: false,
            },
        )
    }

    /// Convenience: a failed response with the given stderr.
    pub fn with_failure(self, args: &[&str], stderr: &str) -> Self {
        self.with_response(
            args,
            GitOutput {
                success: false,
                stdout: String::new(),
                stderr: stderr.to_string(),
                truncated: false,
            },
        )
    }

    /// The output returned for any unmatched call. Defaults to "not a repo".
    pub fn with_default(mut self, output: GitOutput) -> Self {
        self.default = Some(output);
        self
    }

    /// Every argument vector this fake handled, in call order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("fake git calls").clone()
    }
}

impl GitHost for FakeGitHost {
    fn run(&self, args: &[&str], _cwd: &Path) -> Result<GitOutput> {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        self.calls
            .lock()
            .expect("fake git calls")
            .push(args.clone());
        let responses = self.responses.lock().expect("fake git responses");
        if let Some((_, output)) = responses.iter().find(|(expected, _)| *expected == args) {
            return Ok(output.clone());
        }
        drop(responses);
        Ok(self.default.clone().unwrap_or(GitOutput {
            success: false,
            stdout: String::new(),
            stderr: "fatal: not a git repository".to_string(),
            truncated: false,
        }))
    }
}

/// Whether a path points at an executable regular file.
pub fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_host_resolves_registered_programs() {
        let host = FakeProcessHost::new()
            .with_program("opencode", "/opt/opencode")
            .with_version("/opt/opencode", "1.18.31");
        assert_eq!(
            host.find_in_path("opencode"),
            Some(PathBuf::from("/opt/opencode"))
        );
        assert_eq!(host.find_in_path("missing"), None);
        assert_eq!(host.version(Path::new("/opt/opencode")).unwrap(), "1.18.31");
        assert!(host.version(Path::new("/missing")).is_err());
    }

    #[test]
    fn fake_host_records_and_applies_upgrades() {
        let host = FakeProcessHost::new()
            .with_default_version("1.18.20")
            .with_upgrade_success("/opt/opencode", Some("1.18.31"));
        assert_eq!(host.version(Path::new("/opt/opencode")).unwrap(), "1.18.20");
        host.upgrade(Path::new("/opt/opencode")).unwrap();
        assert_eq!(host.version(Path::new("/opt/opencode")).unwrap(), "1.18.31");
        assert_eq!(host.upgrade_calls(), vec![PathBuf::from("/opt/opencode")]);
    }

    #[cfg(unix)]
    #[test]
    fn system_host_rejects_failed_version_commands() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("broken-version");
        std::fs::write(&program, "#!/bin/sh\necho 9.9.9\nexit 1\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SystemProcessHost.version(&program).is_err());
    }

    #[test]
    fn bounded_reads_stop_and_report_truncation() {
        let (out, truncated) = read_bounded(std::io::Cursor::new(b"abcdef".to_vec()), 3);
        assert_eq!(out, b"abc");
        assert!(truncated);

        let (out, truncated) = read_bounded(std::io::Cursor::new(b"ab".to_vec()), 5);
        assert_eq!(out, b"ab");
        assert!(!truncated);

        // The drain variant keeps reading to EOF but retains only the prefix.
        let (out, truncated) = read_drain_bounded(std::io::Cursor::new(b"abcdef".to_vec()), 3);
        assert_eq!(out, b"abc");
        assert!(truncated);

        let (out, truncated) = read_drain_bounded(std::io::Cursor::new(b"ab".to_vec()), 5);
        assert_eq!(out, b"ab");
        assert!(!truncated);
    }

    #[test]
    fn fake_git_run_bounded_truncates_deterministically() {
        let fake = FakeGitHost::new().with_stdout(&["big"], "0123456789");
        let output = fake.run_bounded(&["big"], Path::new("."), 4).unwrap();
        assert_eq!(output.stdout, "0123");
        assert!(output.truncated);
    }
}
