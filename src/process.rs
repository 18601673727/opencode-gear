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
}
