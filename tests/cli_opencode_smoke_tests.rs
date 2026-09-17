//! No-paid smoke tests for the generated orchestration plugin.
//!
//! The generated config is accepted by a *fake* `opencode` executable and, when
//! a real `opencode` is on `PATH`, by `opencode debug config`. Neither path
//! calls a model, network or provider; `debug config` only resolves the config
//! and loads the plugin module (which performs no bridge call until a hook
//! fires).

mod common;

use common::{load_embedded_effective, TestDir};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn project(dir: &TestDir) -> std::path::PathBuf {
    let project = dir.project();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/parser.rs"),
        "pub fn parse() -> u32 { 1 }\n",
    )
    .unwrap();
    project
}

fn generated_config(project: &Path) -> String {
    // Materialize the adapter the way `launch` would, then generate the config.
    opencode_gear::orchestration::plugin::materialize(project).unwrap();
    let effective = load_embedded_effective(project);
    let config = opencode_gear::build::build_opencode_config(&effective, "low").unwrap();
    assert!(
        opencode_gear::orchestration::plugin::has_ocg_plugin(&config),
        "the generated config must reference the adapter"
    );
    serde_json::to_string(&config).unwrap()
}

#[test]
fn fake_opencode_executable_accepts_the_generated_config() {
    let dir = TestDir::new();
    let project = project(&dir);
    let content = generated_config(&project);

    let fake = dir.join("fake-opencode");
    fs::write(
        &fake,
        "#!/bin/sh\n\
         # Fake `opencode debug config`: validate the injected plugin, call no model.\n\
         case \"${OPENCODE_CONFIG_CONTENT:-}\" in\n\
           *ocg-orchestration.js*) printf '%s' \"$OPENCODE_CONFIG_CONTENT\"; exit 0 ;;\n\
           *) echo 'plugin missing from config' >&2; exit 1 ;;\n\
         esac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let output = Command::new(&fake)
        .current_dir(&project)
        .env("OPENCODE_CONFIG_CONTENT", &content)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fake opencode rejected the config: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ocg-orchestration.js"));
}

#[test]
fn real_opencode_debug_config_accepts_the_plugin_when_available() {
    let dir = TestDir::new();
    let project = project(&dir);
    let content = generated_config(&project);

    let available = Command::new("opencode")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !available {
        eprintln!("skipping: opencode is not on PATH");
        return;
    }

    let output = Command::new("opencode")
        .args(["debug", "config"])
        .current_dir(&project)
        .env("OPENCODE_CONFIG_CONTENT", &content)
        .env_remove("OPENCODE_CONFIG")
        .output()
        .expect("run opencode debug config");
    assert!(
        output.status.success(),
        "opencode debug config failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let resolved: Value = serde_json::from_slice(&output.stdout).expect("resolved config JSON");
    let plugins: Vec<String> = resolved
        .get("plugin")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        plugins
            .iter()
            .any(|plugin| plugin.contains("ocg-orchestration.js")),
        "resolved plugin list did not include the adapter: {plugins:?}"
    );
}
