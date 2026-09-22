//! Raw persistence of the latest completed root Lead output.
//!
//! This is deliberately not a report *subsystem*: it is one small, fixed
//! artifact. When OpenCode V2 finishes a root Lead assistant message, the
//! generated adapter reports the raw user-visible text to the bridge and this
//! module writes exactly that text — no headers, timestamps, ids, metadata,
//! summaries or wrapper prose — to
//! `<project>/.opencode-gear/reports/latest-lead-output.md`.
//!
//! Rules:
//!
//! - only a *completed* root Lead message is written (the adapter decides
//!   completion; a streaming partial, an errored/interrupted message and every
//!   worker or non-OCG agent are never reported);
//! - the text is stored byte-verbatim, so a reader can diff it directly;
//! - the file is replaced atomically (temp file + rename) and an interrupted
//!   write never truncates the previous completed output;
//! - every failure is soft: a broken report must never break a session, so
//!   callers report the error and continue.
//!
//! The feature is configured by `reports.latestLeadOutput.enabled` (default
//! true). OpenCode V1 has no event stream for this, so the capture is V2-only;
//! on V1 the switch is inert and no file is produced.

use crate::error::{GearError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The fixed file name under `.opencode-gear/reports/`.
pub const LATEST_LEAD_OUTPUT_FILE: &str = "latest-lead-output.md";

/// `reports.latestLeadOutput` policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LatestLeadOutputConfig {
    /// Persist the latest completed root Lead output.
    pub enabled: bool,
}

impl Default for LatestLeadOutputConfig {
    fn default() -> Self {
        // Enabled by default: the artifact is local, ignored and bounded.
        Self { enabled: true }
    }
}

/// `reports` policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ReportsConfig {
    pub latest_lead_output: LatestLeadOutputConfig,
}

impl ReportsConfig {
    /// Parse `data["reports"]`, falling back to the defaults when absent.
    pub fn from_config(data: &Value) -> Result<Self> {
        let Some(value) = data.get("reports") else {
            return Ok(Self::default());
        };
        if value.is_null() {
            return Ok(Self::default());
        }
        let object = value
            .as_object()
            .ok_or_else(|| GearError::config("reports must be a JSON object"))?;
        let mut config = Self::default();
        if let Some(value) = object.get("latestLeadOutput") {
            if value.is_null() {
                return Ok(config);
            }
            let entry = value
                .as_object()
                .ok_or_else(|| GearError::config("reports.latestLeadOutput must be an object"))?;
            if let Some(enabled) = entry.get("enabled") {
                config.latest_lead_output.enabled = enabled.as_bool().ok_or_else(|| {
                    GearError::config("reports.latestLeadOutput.enabled must be a boolean")
                })?;
            }
        }
        Ok(config)
    }

    /// Collect every policy problem for whole-configuration validation.
    pub fn validate(data: &Value) -> Vec<String> {
        match Self::from_config(data) {
            Ok(_) => Vec::new(),
            Err(error) => vec![error.to_string()],
        }
    }
}

/// `<project>/.opencode-gear/reports`.
pub fn reports_dir(root: &Path) -> PathBuf {
    root.join(".opencode-gear").join("reports")
}

/// `<project>/.opencode-gear/reports/latest-lead-output.md`.
pub fn latest_lead_output_path(root: &Path) -> PathBuf {
    reports_dir(root).join(LATEST_LEAD_OUTPUT_FILE)
}

/// Write the raw text atomically and return the path that was written.
///
/// The text is stored byte-verbatim. The previous file is only replaced once
/// the new content is fully on disk (temp file + rename in the same
/// directory), so a failed or interrupted write cannot leave a truncated
/// report behind.
pub fn write_latest_lead_output(root: &Path, text: &str) -> Result<PathBuf> {
    let dir = reports_dir(root);
    std::fs::create_dir_all(&dir)
        .map_err(|error| GearError::io(format!("cannot create {}", dir.display()), error))?;
    let target = dir.join(LATEST_LEAD_OUTPUT_FILE);
    let temp = dir.join(format!(
        ".{LATEST_LEAD_OUTPUT_FILE}.tmp-{}",
        std::process::id()
    ));
    std::fs::write(&temp, text).map_err(|error| GearError::write(&temp, error))?;
    std::fs::rename(&temp, &target).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        GearError::write(&target, error)
    })?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn absent_reports_config_is_enabled_by_default() {
        let config = ReportsConfig::from_config(&json!({})).unwrap();
        assert!(config.latest_lead_output.enabled);
        assert_eq!(config, ReportsConfig::default());
    }

    #[test]
    fn the_switch_can_disable_the_capture() {
        let config = ReportsConfig::from_config(&json!({
            "reports": {"latestLeadOutput": {"enabled": false}}
        }))
        .unwrap();
        assert!(!config.latest_lead_output.enabled);
    }

    #[test]
    fn malformed_reports_config_is_rejected() {
        for bad in [
            json!({"reports": "on"}),
            json!({"reports": {"latestLeadOutput": true}}),
            json!({"reports": {"latestLeadOutput": {"enabled": "yes"}}}),
        ] {
            assert!(ReportsConfig::from_config(&bad).is_err(), "{bad}");
            assert!(!ReportsConfig::validate(&bad).is_empty(), "{bad}");
        }
        assert!(ReportsConfig::validate(&json!({})).is_empty());
    }

    #[test]
    fn the_path_is_fixed_under_the_ignored_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = latest_lead_output_path(dir.path());
        assert_eq!(
            path,
            dir.path()
                .join(".opencode-gear")
                .join("reports")
                .join("latest-lead-output.md")
        );
    }

    #[test]
    fn the_text_is_stored_byte_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let text = "# Title\n\n- item\n\n```rust\nfn main() {}\n```\n";
        let path = write_latest_lead_output(dir.path(), text).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
    }

    #[test]
    fn a_second_write_replaces_the_first_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        write_latest_lead_output(dir.path(), "first\n").unwrap();
        let path = write_latest_lead_output(dir.path(), "second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        let entries: Vec<String> = std::fs::read_dir(reports_dir(dir.path()))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![LATEST_LEAD_OUTPUT_FILE.to_string()]);
    }

    #[test]
    fn a_blocked_report_path_fails_without_touching_the_previous_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_latest_lead_output(dir.path(), "keep me\n").unwrap();
        // Replace the reports directory with a file: the next write must fail
        // soft and the previous output must survive byte-for-byte.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(reports_dir(dir.path())).unwrap();
        std::fs::write(reports_dir(dir.path()), "not a directory").unwrap();
        assert!(write_latest_lead_output(dir.path(), "must not appear\n").is_err());
        assert_eq!(
            std::fs::read_to_string(reports_dir(dir.path())).unwrap(),
            "not a directory"
        );
    }
}
