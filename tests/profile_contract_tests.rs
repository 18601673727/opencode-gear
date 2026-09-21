//! Regression coverage for the Lead profile contract and the historical
//! "stuck on medium" failure mode.
//!
//! The contract under test: a throttle level resolves, through the whole
//! pipeline (embedded defaults -> effective config -> generated OpenCode config
//! -> exported runtime contract), to exactly one Lead model + reasoning variant,
//! and the Worker Router never follows the throttle.
//!
//! These are library-level tests. The CLI-level complement (that the dry-run
//! output equals the contract the bridge actually exports) lives in
//! `cli_tests.rs` and `orchestration/plugin.rs`.

mod common;

use common::{load_embedded_effective, write_yaml, TestDir};
use opencode_gear::{build, model};
use serde_json::{json, Value};

/// The shipped profile. Kept as data so every assertion shares one source.
const PROFILES: [(&str, &str, &str); 3] = [
    ("low", "openai/gpt-5.6-sol", "low"),
    ("mid", "openai/gpt-5.6-sol", "medium"),
    ("high", "openai/gpt-6-astra", "low"),
];

fn lead_agent(config: &Value, level: &str) -> Value {
    config["agent"][model::lead_agent_id(level)].clone()
}

#[test]
fn embedded_defaults_define_the_documented_profile() {
    let dir = TestDir::new();
    let project = dir.project();
    let effective = load_embedded_effective(&project);
    assert_eq!(
        model::resolve_throttle(&effective.data, None, None),
        "low",
        "the shipped default throttle must be low"
    );
    for (level, full, variant) in PROFILES {
        let contract = model::lead_contract(&effective.data, level).unwrap();
        assert_eq!(contract.level, level);
        assert_eq!(contract.agent, model::lead_agent_id(level));
        assert_eq!(contract.full_model_id(), full, "{level}");
        assert_eq!(contract.variant.as_deref(), Some(variant), "{level}");
    }
}

#[test]
fn generated_config_matches_the_contract_for_every_level() {
    let dir = TestDir::new();
    let project = dir.project();
    let effective = load_embedded_effective(&project);
    for (level, full, variant) in PROFILES {
        let contract = model::lead_contract(&effective.data, level).unwrap();
        let config = build::build_opencode_config(&effective, level).unwrap();
        assert_eq!(config["default_agent"], json!(contract.agent), "{level}");
        assert_eq!(config["model"], json!(full), "{level}");
        let agent = lead_agent(&config, level);
        assert_eq!(agent["mode"], json!("primary"), "{level}");
        assert_eq!(agent["model"], json!(full), "{level}");
        assert_eq!(agent["variant"], json!(variant), "{level}");
        assert_eq!(contract.full_model_id(), full, "{level}");
    }
}

#[test]
fn switching_levels_never_retains_a_previous_contract() {
    let dir = TestDir::new();
    let project = dir.project();
    let effective = load_embedded_effective(&project);
    // The historical bug: the runtime stayed on `medium` after the profile
    // changed. Re-resolving in sequence must always yield the requested level,
    // including a return to a level seen earlier.
    for (level, full, variant) in [
        ("low", "openai/gpt-5.6-sol", "low"),
        ("mid", "openai/gpt-5.6-sol", "medium"),
        ("high", "openai/gpt-6-astra", "low"),
        ("low", "openai/gpt-5.6-sol", "low"),
        ("high", "openai/gpt-6-astra", "low"),
    ] {
        let config = build::build_opencode_config(&effective, level).unwrap();
        assert_eq!(config["default_agent"], json!(model::lead_agent_id(level)));
        assert_eq!(config["model"], json!(full), "{level}");
        let agent = lead_agent(&config, level);
        assert_eq!(agent["model"], json!(full), "{level}");
        assert_eq!(agent["variant"], json!(variant), "{level}");
    }
}

#[test]
fn worker_agents_do_not_follow_the_throttle() {
    let dir = TestDir::new();
    let project = dir.project();
    let effective = load_embedded_effective(&project);
    let low = build::build_opencode_config(&effective, "low").unwrap();
    let high = build::build_opencode_config(&effective, "high").unwrap();
    for role in opencode_gear::defaults::WORKER_ROLES {
        let id = model::worker_agent_id(role);
        assert_eq!(
            low["agent"][id.as_str()],
            high["agent"][id.as_str()],
            "{role} changed with the throttle"
        );
    }
    // Selecting a level only changes the default agent and the top-level
    // model; the full per-level Lead map is emitted every time, and the plugin
    // enforces the selected contract at runtime.
    assert_ne!(low["default_agent"], high["default_agent"]);
    assert_ne!(low["model"], high["model"]);
    assert_ne!(low["agent"]["lead-low"], low["agent"]["lead-mid"]);
    assert_ne!(low["agent"]["lead-low"], low["agent"]["lead-high"]);
}

#[test]
fn project_override_changes_only_the_overridden_field() {
    let dir = TestDir::new();
    let project = dir.project();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"high": {"variant": "xhigh"}}}}),
    );
    let effective = load_embedded_effective(&project);
    let high = model::lead_contract(&effective.data, "high").unwrap();
    assert_eq!(high.full_model_id(), "openai/gpt-6-astra");
    assert_eq!(high.variant.as_deref(), Some("xhigh"));
    // The un-overridden levels keep the shipped contract.
    assert_eq!(
        model::lead_contract(&effective.data, "low")
            .unwrap()
            .variant
            .as_deref(),
        Some("low")
    );

    // Absence of the override falls back to the shipped contract.
    let plain_project = dir.join("plain");
    std::fs::create_dir_all(&plain_project).unwrap();
    let plain = load_embedded_effective(&plain_project);
    assert_eq!(
        model::lead_contract(&plain.data, "high")
            .unwrap()
            .variant
            .as_deref(),
        Some("low")
    );
}

/// A Lead on a custom provider that exposes no reasoning variants must resolve
/// to `None`, and the generated OpenCode config must omit `variant` entirely.
#[test]
fn custom_provider_without_a_variant_is_provider_default() {
    let dir = TestDir::new();
    let project = dir.project();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"high": {"model": "kimi-k3", "variant": null}}}}),
    );
    let effective = load_embedded_effective(&project);
    assert_eq!(
        opencode_gear::validate::validate(&effective),
        Vec::<String>::new()
    );

    let high = model::lead_contract(&effective.data, "high").unwrap();
    assert_eq!(high.full_model_id(), "volcengine-coding-plan/kimi-k3");
    assert_eq!(high.variant, None);

    let config = build::build_opencode_config(&effective, "high").unwrap();
    let agent = lead_agent(&config, "high");
    assert_eq!(agent["model"], json!("volcengine-coding-plan/kimi-k3"));
    assert!(
        agent.get("variant").is_none(),
        "provider-default must not fabricate a variant: {agent}"
    );
}

/// V1 (generated OpenCode config) and V2 (exported runtime contract) must both
/// omit an absent variant rather than writing null, empty or a fake value.
#[test]
fn absent_variant_is_omitted_from_config_and_contract() {
    let dir = TestDir::new();
    let project = dir.project();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"high": {"model": "kimi-k3", "variant": null}}}}),
    );
    let effective = load_embedded_effective(&project);

    // V1: the generated config has no `variant` key at all.
    let config = build::build_opencode_config(&effective, "high").unwrap();
    assert!(config["agent"]["lead-high"].get("variant").is_none());
    assert_eq!(
        config["agent"]["lead-high"]["model"],
        json!("volcengine-coding-plan/kimi-k3")
    );

    // V2: the exported contract serializes without a `variant` key.
    let contract = model::lead_contract(&effective.data, "high").unwrap();
    let serialized = serde_json::to_value(&contract).unwrap();
    assert_eq!(serialized["provider_id"], json!("volcengine-coding-plan"));
    assert_eq!(serialized["model_id"], json!("kimi-k3"));
    assert!(
        serialized.get("variant").is_none(),
        "exported contract must not fabricate a variant: {serialized}"
    );
}
