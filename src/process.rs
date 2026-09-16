//! Centralized external process execution.
//!
//! This module is the only place that builds a [`std::process::Command`], so
//! every child process the tool ever starts goes through one auditable path.

use crate::error::{GearError, Result};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

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
