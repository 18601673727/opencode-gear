//! Library-level tests for the configuration pipeline (parity with the
//! historical Python suite, expressed against the Rust API).

mod common;

use common::*;
use opencode_gear::config::{build_effective, Effective};
use opencode_gear::defaults::{load_defaults, GearSource, EXECUTION_TIERS};
use opencode_gear::{build, model, observability, prompt, validate};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn setup() -> (TestDir, PathBuf, PathBuf) {
    let dir = TestDir::new();
    let home = gear_home(&dir);
    let project = dir.project();
    (dir, home, project)
}

fn disk(home: &Path, project: &Path) -> Effective {
    load_disk_effective(home, project, None, None)
}

fn lead_prompt(home: &Path) -> String {
    fs::read_to_string(home.join("config").join("prompts").join("lead.md")).expect("lead prompt")
}

fn worker_model(effective: &Effective, level: &str, role: &str) -> String {
    let config = build::build_opencode_config(effective, level).expect("build");
    config["agent"][model::worker_agent_id(role)]["model"]
        .as_str()
        .expect("model")
        .to_string()
}

#[test]
fn default_throttle_is_low() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    assert_eq!(model::resolve_throttle(&effective.data, None, None), "low");
}

#[test]
fn exactly_three_levels_are_shipped() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let levels = effective.data["throttle"]["levels"]
        .as_object()
        .expect("levels");
    assert_eq!(levels.len(), EXECUTION_TIERS.len());
    for level in EXECUTION_TIERS {
        assert!(levels.contains_key(level), "missing level {level}");
    }
}

#[test]
fn execution_tiers_map_to_the_expected_models() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let level = |name: &str| effective.data["throttle"]["levels"][name]["model"].clone();
    assert_eq!(
        model::model_full_id(&effective.data, level("low").as_str().unwrap())
            .unwrap()
            .1,
        "openai/gpt-5.6-sol"
    );
    assert_eq!(
        effective.data["throttle"]["levels"]["low"]["variant"],
        json!("low")
    );
    assert_eq!(
        model::model_full_id(&effective.data, level("mid").as_str().unwrap())
            .unwrap()
            .1,
        "openai/gpt-5.6-sol"
    );
    assert_eq!(
        effective.data["throttle"]["levels"]["mid"]["variant"],
        json!("medium")
    );
    assert_eq!(
        model::model_full_id(&effective.data, level("high").as_str().unwrap())
            .unwrap()
            .1,
        "openai/gpt-6-astra"
    );
    assert_eq!(
        effective.data["throttle"]["levels"]["high"]["variant"],
        json!("low")
    );
}

#[test]
fn one_lead_agent_per_level() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    for level in EXECUTION_TIERS {
        let config = build::build_opencode_config(&effective, level).expect("build");
        assert_eq!(config["default_agent"], json!(model::lead_agent_id(level)));
        let agent = &config["agent"][model::lead_agent_id(level)];
        assert_eq!(agent["mode"], json!("primary"));
        assert!(agent["prompt"].as_str().unwrap().contains("OpenCode Gear"));
    }
}

#[test]
fn unknown_level_is_rejected() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    assert!(build::build_opencode_config(&effective, "turbo").is_err());
}

#[test]
fn throttle_precedence_is_cli_env_default() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    assert_eq!(
        model::resolve_throttle(&effective.data, None, Some("high")),
        "high"
    );
    assert_eq!(
        model::resolve_throttle(&effective.data, Some("mid"), Some("high")),
        "mid"
    );
    assert_eq!(model::resolve_throttle(&effective.data, None, None), "low");
}

#[test]
fn project_override_can_change_the_default_throttle() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"default": "mid"}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(model::resolve_throttle(&effective.data, None, None), "mid");
}

#[test]
fn throttle_does_not_change_worker_routing() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let roles: Vec<String> = model::role_specs(&effective.data)
        .unwrap()
        .keys()
        .cloned()
        .collect();
    for role in &roles {
        let baseline = worker_model(&effective, "low", role);
        for level in EXECUTION_TIERS {
            assert_eq!(
                worker_model(&effective, level, role),
                baseline,
                "{role} changed under throttle {level}"
            );
        }
    }
}

#[test]
fn shipped_routes_are_stable() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let full = |role: &str| -> String {
        let key = effective.data["routing"]["roles"][role]["model"]
            .as_str()
            .expect("model key");
        model::model_full_id(&effective.data, key).unwrap().1
    };
    assert_eq!(full("explore"), "volcengine-coding-plan/kimi-k2.7-code");
    assert_eq!(full("explore-deep"), "volcengine-coding-plan/kimi-k3");
    assert_eq!(full("build"), "opencode-go/deepseek-v4.1-flash");
    assert_eq!(full("verify"), "opencode-go/glm-5.3-flash");
    assert_eq!(full("debug"), "opencode-go/glm-5.3");
    assert_eq!(full("docs"), "opencode-go/deepseek-v4.1-flash");
}

#[test]
fn provider_binding_is_deterministic() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    for (role, spec) in model::role_specs(&effective.data).unwrap() {
        let key = spec["model"].as_str().unwrap();
        let (provider, _) = model::model_full_id(&effective.data, key).unwrap();
        if role.starts_with("explore") {
            assert_eq!(provider, "volcengine-coding-plan");
        } else {
            assert_eq!(provider, "opencode-go");
        }
        assert_ne!(provider, "openai", "OpenAI must not be a worker");
    }
}

#[test]
fn worker_agents_are_hidden_and_cannot_delegate() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    for role in model::role_specs(&effective.data).unwrap().keys() {
        let agent = &config["agent"][model::worker_agent_id(role)];
        assert_eq!(agent["mode"], json!("subagent"));
        assert_eq!(agent["hidden"], json!(true));
        assert_eq!(agent["permission"]["task"], json!("deny"));
    }
}

#[test]
fn lead_can_only_task_its_own_workers() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let task = &config["agent"]["lead-low"]["permission"]["task"];
    assert_eq!(task["*"], json!("deny"));
    let allowed: Vec<String> = task
        .as_object()
        .unwrap()
        .iter()
        .filter(|(_, value)| value.as_str() == Some("allow"))
        .map(|(key, _)| key.clone())
        .collect();
    let expected: Vec<String> = model::role_specs(&effective.data)
        .unwrap()
        .keys()
        .map(|role| model::worker_agent_id(role))
        .collect();
    assert_eq!(allowed.len(), expected.len());
    for agent in expected {
        assert!(allowed.contains(&agent));
    }
}

#[test]
fn enabled_providers_are_the_routing_providers() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let providers: Vec<String> = config["enabled_providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        providers,
        vec!["openai", "volcengine-coding-plan", "opencode-go"]
    );
}

#[test]
fn fallback_is_reported_and_validated() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {
            "model": "deepseek-v4.1-flash",
            "variant": "high",
            "fallback": [{"model": "glm-5.3", "variant": "high"}]
        }}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
    let block = prompt::routing_block(&effective).expect("routing block");
    assert!(block.contains("Configured fallbacks"));
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let lead = config["agent"]["lead-low"]["prompt"].as_str().unwrap();
    assert!(lead.contains("Configured fallbacks"));
}

#[test]
fn default_config_is_valid() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn missing_model_key_is_reported() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {"model": "no-such-model"}}}}),
    );
    let effective = disk(&home, &project);
    let errors = validate::validate(&effective);
    assert!(
        errors.iter().any(|error| error.contains("no-such-model")),
        "{errors:?}"
    );
}

#[test]
fn missing_provider_is_reported() {
    let (_dir, home, project) = setup();
    patch_yaml(&home.join("config").join("models.yaml"), |value| {
        value["models"]["mystery"] = json!({"provider": "no-such-provider", "id": "mystery-1"});
    });
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {"model": "mystery"}}}}),
    );
    let effective = disk(&home, &project);
    let errors = validate::validate(&effective);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("no-such-provider")),
        "{errors:?}"
    );
}

#[test]
fn unknown_variant_is_reported() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {
            "model": "deepseek-v4.1-flash", "variant": "impossible"
        }}}}),
    );
    let effective = disk(&home, &project);
    let errors = validate::validate(&effective);
    assert!(
        errors.iter().any(|error| error.contains("impossible")),
        "{errors:?}"
    );
}

#[test]
fn valid_variant_is_accepted() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {
            "model": "deepseek-v4.1-flash", "variant": "max"
        }}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn invalid_lead_variant_is_rejected() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"mid": {"variant": "impossible"}}}}),
    );
    let effective = disk(&home, &project);
    let errors = validate::validate(&effective);
    assert!(
        errors.iter().any(|error| error.contains("impossible")),
        "{errors:?}"
    );
}

#[test]
fn lead_without_a_variant_is_valid() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"mid": {"model": "kimi-k3", "variant": null}}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
    let contract = model::lead_contract(&effective.data, "mid").unwrap();
    assert_eq!(contract.variant, None);
}

#[test]
fn unknown_throttle_model_is_reported() {
    let (_dir, home, project) = setup();
    patch_yaml(&home.join("config").join("throttle.yaml"), |value| {
        value["levels"]["mid"]["model"] = json!("ghost");
    });
    let effective = disk(&home, &project);
    assert!(validate::validate(&effective)
        .iter()
        .any(|error| error.contains("ghost")));
}

#[test]
fn missing_prompt_is_reported() {
    let (_dir, home, project) = setup();
    fs::remove_file(home.join("config").join("prompts").join("docs.md")).expect("remove prompt");
    let effective = disk(&home, &project);
    assert!(validate::validate(&effective)
        .iter()
        .any(|error| error.contains("docs")));
}

#[test]
fn require_valid_raises() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {"model": "no-such-model"}}}}),
    );
    let effective = disk(&home, &project);
    assert!(validate::require_valid(&effective).is_err());
}

#[test]
fn missing_config_file_is_reported() {
    let (dir, home, _project) = setup();
    fs::remove_file(home.join("config").join("models.yaml")).expect("remove models");
    assert!(load_defaults(&GearSource::Dir(home)).is_err());
    drop(dir);
}

#[test]
fn project_override_swaps_a_model() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"models": {"models": {"glm-5.3": {
            "provider": "opencode-go", "id": "glm-5.3", "label": "GLM-5.3 custom"
        }}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(
        model::model_label(&effective.data, "glm-5.3"),
        "GLM-5.3 custom"
    );
}

#[test]
fn project_override_beats_user_override() {
    let (dir, home, project) = setup();
    let user = dir.join("user.yaml");
    write_yaml(
        &user,
        &json!({"routing": {"roles": {"build": {"model": "glm-5.3"}}}}),
    );
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"routing": {"roles": {"build": {"model": "glm-5.3-flash"}}}}),
    );
    let effective = load_disk_effective(&home, &project, Some(&user), None);
    assert_eq!(
        effective.data["routing"]["roles"]["build"]["model"],
        json!("glm-5.3-flash")
    );
}

#[test]
fn user_override_is_used_when_project_is_absent() {
    let (dir, home, project) = setup();
    let user = dir.join("user.yaml");
    write_yaml(
        &user,
        &json!({"routing": {"roles": {"build": {"model": "glm-5.3"}}}}),
    );
    let effective = load_disk_effective(&home, &project, Some(&user), None);
    assert_eq!(
        effective.data["routing"]["roles"]["build"]["model"],
        json!("glm-5.3")
    );
    let names: Vec<&str> = effective.applied.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, vec!["user"]);
}

#[test]
fn missing_override_files_are_ignored() {
    let (dir, home, project) = setup();
    let effective = load_disk_effective(
        &home,
        &project,
        Some(&dir.join("nope-user.yaml")),
        Some(&dir.join("nope-project.yaml")),
    );
    assert!(effective.applied.is_empty());
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn project_override_can_replace_a_prompt() {
    let (dir, home, project) = setup();
    let prompt_path = dir.join("custom-lead.md");
    fs::write(&prompt_path, "Custom lead prompt.\n").expect("write prompt");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": prompt_path.to_string_lossy()}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read prompt");
    assert_eq!(body, "Custom lead prompt.");
}

#[test]
fn raw_opencode_override_is_merged_last() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"opencode": {"username": "gearbox"}}),
    );
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    assert_eq!(config["username"], json!("gearbox"));
}

#[test]
fn relative_prompt_path_resolves_against_project() {
    let (_dir, home, project) = setup();
    fs::write(project.join("lead.md"), "Relative lead prompt.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": "lead.md"}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert_eq!(body, "Relative lead prompt.");
}

#[test]
fn append_keeps_core_prompt_and_adds_policy() {
    let (_dir, home, project) = setup();
    let policy = project.join("lead-policy.md");
    fs::write(&policy, "# Project policy\n\nNever touch production.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": [policy.to_string_lossy()]}}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert!(body.contains("You are the Lead in an OpenCode Gear multi-model setup."));
    assert!(body.contains("Never touch production."));
    assert!(body.contains("---"));
    assert!(body.find("You are the Lead").unwrap() < body.find("Never touch production.").unwrap());
}

#[test]
fn append_only_uses_the_gear_default_not_a_replacement() {
    let (_dir, home, project) = setup();
    let policy = project.join("policy.md");
    fs::write(&policy, "Project-only clause.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": [policy.to_string_lossy()]}}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert!(body.starts_with(lead_prompt(&home).trim()));
    assert!(body.ends_with("Project-only clause."));
}

#[test]
fn append_accepts_inline_text() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": [{"text": "Inline clause."}]}}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert!(body.contains("Inline clause."));
}

#[test]
fn path_then_append_replaces_and_extends() {
    let (_dir, home, project) = setup();
    let replacement = project.join("replacement.md");
    fs::write(&replacement, "Replacement core.\n").expect("write");
    let extra = project.join("extra.md");
    fs::write(&extra, "Extra policy.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {
            "path": replacement.to_string_lossy(),
            "append": [extra.to_string_lossy()]
        }}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert!(body.starts_with("Replacement core."));
    assert!(body.ends_with("Extra policy."));
    assert!(!body.contains("You are the Lead in an OpenCode Gear"));
}

#[test]
fn append_accepts_relative_project_path() {
    let (_dir, home, project) = setup();
    fs::create_dir_all(project.join("policy")).expect("mkdir");
    fs::write(
        project.join("policy").join("lead.md"),
        "Relative project policy.\n",
    )
    .expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": ["policy/lead.md"]}}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read");
    assert!(body.ends_with("Relative project policy."));
}

#[test]
fn appended_policy_reaches_the_rendered_lead_agent() {
    let (_dir, home, project) = setup();
    let policy = project.join("lead-policy.md");
    fs::write(&policy, "Use `{{build}}` only for approved scope.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": [policy.to_string_lossy()]}}}),
    );
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let text = config["agent"]["lead-low"]["prompt"].as_str().unwrap();
    assert!(text.contains("Use `ocg-build` only for approved scope."));
}

#[test]
fn missing_appended_file_is_reported() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": ["missing-policy.md"]}}}),
    );
    let effective = disk(&home, &project);
    assert!(validate::validate(&effective)
        .iter()
        .any(|error| error.contains("lead")));
}

#[test]
fn append_must_be_a_list() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": "not-a-list"}}}),
    );
    let effective = disk(&home, &project);
    assert!(validate::validate(&effective)
        .iter()
        .any(|error| error.contains("append")));
}

#[test]
fn append_does_not_leak_into_worker_prompts() {
    let (_dir, home, project) = setup();
    let policy = project.join("lead-policy.md");
    fs::write(&policy, "Lead-only clause.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"prompts": {"lead": {"append": [policy.to_string_lossy()]}}}),
    );
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    for role in model::role_specs(&effective.data).unwrap().keys() {
        let text = config["agent"][model::worker_agent_id(role)]["prompt"]
            .as_str()
            .unwrap();
        assert!(!text.contains("Lead-only clause."));
    }
}

#[test]
fn core_lead_prompt_stays_project_agnostic() {
    let (_dir, home, _project) = setup();
    let core = lead_prompt(&home);
    for token in [
        concat!("Zh", "uju"),
        concat!("xiang", "min"),
        concat!("chun", "cheon"),
    ] {
        assert!(!core.contains(token), "core prompt leaked {token}");
    }
    assert!(!core.contains("/home/"));
    assert!(!core.contains("/Users/"));
}

#[test]
fn project_override_is_loaded_only_for_that_project() {
    let (dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"default": "high"}}),
    );
    let here = disk(&home, &project);
    let other = dir.join("other-project");
    fs::create_dir_all(&other).expect("mkdir");
    let elsewhere = disk(&home, &other);
    assert_eq!(model::resolve_throttle(&here.data, None, None), "high");
    assert_eq!(model::resolve_throttle(&elsewhere.data, None, None), "low");
    assert_eq!(here.applied.len(), 1);
    assert!(elsewhere.applied.is_empty());
}

#[test]
fn rendered_lead_is_the_core_prompt_without_project_policy() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let rendered = config["agent"]["lead-low"]["prompt"].as_str().unwrap();
    let core = lead_prompt(&home);
    for line in core.lines() {
        let stripped = line.trim();
        if !stripped.is_empty() && !stripped.contains("{{") && !stripped.contains('|') {
            assert!(rendered.contains(stripped), "missing line: {stripped}");
        }
    }
}

#[test]
fn trace_is_disabled_by_default() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    assert!(observability::trace_path(&effective, None).is_none());
    assert!(observability::record_event(&effective, "launch", "low", None).is_none());
}

#[test]
fn trace_writes_local_jsonl_without_content() {
    let (dir, home, project) = setup();
    let trace_file = dir.join("events.jsonl");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"observability": {"enabled": true, "path": trace_file.to_string_lossy()}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(
        observability::record_event(&effective, "launch", "mid", None),
        Some(trace_file.clone())
    );
    let record: serde_json::Value =
        serde_json::from_str(fs::read_to_string(&trace_file).unwrap().trim()).expect("record");
    assert_eq!(record["event"], json!("launch"));
    assert_eq!(record["throttle"], json!("mid"));
    assert_eq!(record["default_agent"], json!("lead-mid"));
    assert_eq!(
        record["routing"]["build"],
        json!("opencode-go/deepseek-v4.1-flash")
    );
    let serialized = record.to_string().to_lowercase();
    assert!(!serialized.contains("prompt"));
    assert!(!serialized.contains("source"));
}

#[test]
fn trace_env_override_is_used() {
    let (dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"observability": {"enabled": true}}),
    );
    let effective = disk(&home, &project);
    let trace_file = dir.join("env-events.jsonl");
    assert_eq!(
        observability::trace_path(&effective, Some(&trace_file)),
        Some(trace_file)
    );
}

#[test]
fn arbitrary_routing_roles_are_supported() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({
            "routing": {"roles": {"audit": {"model": "glm-5.3", "description": "audit role"}}},
            "prompts": {"audit": {"text": "You audit changes."}}
        }),
    );
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let agent = &config["agent"]["ocg-audit"];
    assert_eq!(agent["model"], json!("opencode-go/glm-5.3"));
    assert_eq!(agent["description"], json!("audit role"));
    assert_eq!(agent["prompt"], json!("You audit changes."));
    assert_eq!(
        config["agent"]["lead-low"]["permission"]["task"]["ocg-audit"],
        json!("allow")
    );
    assert!(config["enabled_providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "opencode-go"));
}

#[test]
fn arbitrary_role_placeholders_are_substituted_in_the_lead() {
    let (_dir, home, project) = setup();
    let policy = project.join("policy.md");
    fs::write(&policy, "Delegate audits to `{{audit}}`.\n").expect("write");
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({
            "routing": {"roles": {"audit": {"model": "glm-5.3"}}},
            "prompts": {
                "audit": {"text": "You audit."},
                "lead": {"append": [policy.to_string_lossy()]}
            }
        }),
    );
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    let text = config["agent"]["lead-low"]["prompt"].as_str().unwrap();
    assert!(text.contains("Delegate audits to `ocg-audit`."));
}

#[test]
fn embedded_defaults_match_the_disk_config() {
    let (_dir, home, project) = setup();
    let embedded = load_embedded_effective(&project);
    let disk_effective = disk(&home, &project);
    for level in EXECUTION_TIERS {
        let a = build::build_opencode_config(&embedded, level).expect("embedded build");
        let b = build::build_opencode_config(&disk_effective, level).expect("disk build");
        assert_eq!(a, b, "embedded and disk config differ at level {level}");
    }
}

fn build_with(
    home: &Path,
    project: &Path,
    user_path: &Path,
    project_path: &Path,
) -> opencode_gear::error::Result<Effective> {
    let defaults = load_defaults(&GearSource::Dir(home.to_path_buf())).expect("defaults");
    build_effective(
        defaults,
        Some(home.to_path_buf()),
        project,
        user_path,
        project_path,
        None,
    )
}

#[test]
fn stale_project_json_is_rejected_and_named() {
    let (dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"default": "mid"}}),
    );
    let stale = project.join(".opencode-gear.json");
    fs::write(&stale, "{\"throttle\":{\"default\":\"high\"}}\n").expect("stale json");

    let error = build_with(
        &home,
        &project,
        &dir.join("no-user.yaml"),
        &project.join(".opencode-gear.yaml"),
    )
    .expect_err("stale project JSON must be rejected");
    let text = error.to_string();
    assert!(text.contains(stale.to_str().unwrap()), "{text}");
    assert!(
        text.contains(".opencode-gear.yaml"),
        "the YAML target must be named: {text}"
    );
    assert!(text.to_lowercase().contains("yaml"), "{text}");
    assert!(
        !text.contains("mid") && !text.contains("high"),
        "the stale JSON must not be merged or applied: {text}"
    );
}

#[test]
fn stale_user_json_is_rejected_and_named() {
    let (dir, home, project) = setup();
    let stale = dir.join("user.yaml");
    fs::write(dir.join("user.json"), "throttle:\n  default: high\n").expect("stale json");

    let error = build_with(
        &home,
        &project,
        &stale,
        &project.join(".opencode-gear.yaml"),
    )
    .expect_err("stale user JSON must be rejected");
    let text = error.to_string();
    assert!(text.contains("user.json"), "{text}");
    assert!(text.contains("user.yaml"), "{text}");
    assert!(text.to_lowercase().contains("yaml"), "{text}");
}

#[test]
fn explicit_json_override_is_rejected() {
    let (dir, home, project) = setup();
    let json_path = dir.join("custom.json");
    fs::write(&json_path, "throttle:\n  default: high\n").expect("json override");

    let error = build_with(
        &home,
        &project,
        &json_path,
        &project.join(".opencode-gear.yaml"),
    )
    .expect_err("an explicit JSON override must be rejected");
    let text = error.to_string();
    assert!(text.contains("custom.json"), "{text}");
    assert!(text.contains("custom.yaml"), "{text}");
}

#[test]
fn comment_only_yaml_override_is_a_noop() {
    let (_dir, home, project) = setup();
    fs::write(
        project.join(".opencode-gear.yaml"),
        "# This project has no overrides yet.\n",
    )
    .expect("write override");
    let effective = disk(&home, &project);
    assert_eq!(model::resolve_throttle(&effective.data, None, None), "low");
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn missing_yaml_override_files_are_ignored() {
    let (dir, home, project) = setup();
    let effective = build_with(
        &home,
        &project,
        &dir.join("nope-user.yaml"),
        &dir.join("nope-project.yaml"),
    )
    .expect("missing YAML overrides are fine");
    assert!(effective.applied.is_empty());
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn v2_config_uses_the_2011_local_plugin_discovery_contract() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);

    let v1 = build::build_opencode_config(&effective, "low").expect("v1 build");
    let v2 = build::build_opencode_config_for(
        &effective,
        "low",
        opencode_gear::runtime::compat::v2_adapter(),
    )
    .expect("v2 build");

    // v1 keeps the historical singular plugin array and `task` permission key.
    assert!(v1.get("plugin").is_some(), "v1 must keep `plugin`");
    assert!(v1.get("plugins").is_none());
    assert!(v1["agent"]["lead-low"]["permission"].get("task").is_some());
    assert_eq!(
        v1["agent"]["ocg-build"]["permission"]["task"],
        json!("deny")
    );

    // 2.0.11 discovers local plugins from OPENCODE_CONFIG_DIR/plugins. Its
    // singular `plugin` array is for npm packages, so a generated local file
    // must not be represented as a file:// package entry.
    assert!(v2.get("plugins").is_none(), "2.0.11 has no `plugins` key");
    assert!(
        v2.get("plugin").is_none(),
        "local discovery adds no package entry"
    );

    // v2 renamed the delegation tool/permission key to `subagent`.
    assert!(v2["agent"]["lead-low"]["permission"]
        .get("subagent")
        .is_some());
    assert!(v2["agent"]["lead-low"]["permission"].get("task").is_none());
    assert_eq!(
        v2["agent"]["ocg-build"]["permission"]["subagent"],
        json!("deny")
    );

    // Agent/role structure and the Lead model selection are unchanged.
    assert_eq!(v2["default_agent"], v1["default_agent"]);
    assert_eq!(v2["model"], v1["model"]);
    assert_eq!(
        v2["agent"]["lead-low"]["model"],
        v1["agent"]["lead-low"]["model"]
    );
}

#[test]
fn changed_model_clears_inherited_variant_but_same_model_preserves_it() {
    let (_dir, home, project) = setup();
    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({
            "throttle": {"levels": {"mid": {"model": "glm-5.3"}}},
            "routing": {"roles": {"build": {"model": "glm-5.3"}}}
        }),
    );
    let effective = disk(&home, &project);
    assert!(effective.data["throttle"]["levels"]["mid"]
        .get("variant")
        .is_none());
    assert!(effective.data["routing"]["roles"]["build"]
        .get("variant")
        .is_none());

    write_yaml(
        &project.join(".opencode-gear.yaml"),
        &json!({"throttle": {"levels": {"mid": {"model": "sol"}}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(
        effective.data["throttle"]["levels"]["mid"]["variant"],
        json!("medium")
    );
}
