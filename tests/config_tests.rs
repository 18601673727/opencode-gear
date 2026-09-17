//! Library-level tests for the configuration pipeline (parity with the
//! historical Python suite, expressed against the Rust API).

mod common;

use common::*;
use opencode_gear::config::Effective;
use opencode_gear::defaults::{load_defaults, GearSource, LEAD_LEVELS};
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

fn consumer_model(effective: &Effective, level: &str, role: &str) -> String {
    let config = build::build_opencode_config(effective, level).expect("build");
    config["agent"][model::consumer_agent_id(role)]["model"]
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
    assert_eq!(levels.len(), LEAD_LEVELS.len());
    for level in LEAD_LEVELS {
        assert!(levels.contains_key(level), "missing level {level}");
    }
}

#[test]
fn lead_tiers_map_to_the_expected_models() {
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
        json!("medium")
    );
    assert_eq!(
        model::model_full_id(&effective.data, level("mid").as_str().unwrap())
            .unwrap()
            .1,
        "openai/gpt-5.6-sol"
    );
    assert_eq!(
        effective.data["throttle"]["levels"]["mid"]["variant"],
        json!("high")
    );
    assert_eq!(
        model::model_full_id(&effective.data, level("high").as_str().unwrap())
            .unwrap()
            .1,
        "openai/gpt-6-astra"
    );
    assert_eq!(
        effective.data["throttle"]["levels"]["high"]["variant"],
        json!("high")
    );
}

#[test]
fn one_lead_agent_per_level() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    for level in LEAD_LEVELS {
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
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"throttle": {"default": "mid"}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(model::resolve_throttle(&effective.data, None, None), "mid");
}

#[test]
fn throttle_does_not_change_consumer_routing() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let roles: Vec<String> = model::role_specs(&effective.data)
        .unwrap()
        .keys()
        .cloned()
        .collect();
    for role in &roles {
        let baseline = consumer_model(&effective, "low", role);
        for level in LEAD_LEVELS {
            assert_eq!(
                consumer_model(&effective, level, role),
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
    assert_eq!(full("explore"), "volcengine-coding/kimi-k2.7-code");
    assert_eq!(full("explore-deep"), "volcengine-coding/kimi-k3");
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
            assert_eq!(provider, "volcengine-coding");
        } else {
            assert_eq!(provider, "opencode-go");
        }
        assert_ne!(provider, "openai", "OpenAI must not be a consumer");
    }
}

#[test]
fn consumer_agents_are_hidden_and_cannot_delegate() {
    let (_dir, home, project) = setup();
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    for role in model::role_specs(&effective.data).unwrap().keys() {
        let agent = &config["agent"][model::consumer_agent_id(role)];
        assert_eq!(agent["mode"], json!("subagent"));
        assert_eq!(agent["hidden"], json!(true));
        assert_eq!(agent["permission"]["task"], json!("deny"));
    }
}

#[test]
fn lead_can_only_task_its_own_consumers() {
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
        .map(|role| model::consumer_agent_id(role))
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
        vec!["openai", "volcengine-coding", "opencode-go"]
    );
}

#[test]
fn fallback_is_reported_and_validated() {
    let (_dir, home, project) = setup();
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    patch_json(&home.join("config").join("models.json"), |value| {
        value["models"]["mystery"] = json!({"provider": "no-such-provider", "id": "mystery-1"});
    });
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"routing": {"roles": {"build": {
            "model": "deepseek-v4.1-flash", "variant": "max"
        }}}}),
    );
    let effective = disk(&home, &project);
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn unknown_throttle_model_is_reported() {
    let (_dir, home, project) = setup();
    patch_json(&home.join("config").join("throttle.json"), |value| {
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
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"routing": {"roles": {"build": {"model": "no-such-model"}}}}),
    );
    let effective = disk(&home, &project);
    assert!(validate::require_valid(&effective).is_err());
}

#[test]
fn missing_config_file_is_reported() {
    let (dir, home, _project) = setup();
    fs::remove_file(home.join("config").join("models.json")).expect("remove models");
    assert!(load_defaults(&GearSource::Dir(home)).is_err());
    drop(dir);
}

#[test]
fn project_override_swaps_a_model() {
    let (_dir, home, project) = setup();
    write_json(
        &project.join(".opencode-gear.json"),
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
    let user = dir.join("user.json");
    write_json(
        &user,
        &json!({"routing": {"roles": {"build": {"model": "glm-5.3"}}}}),
    );
    write_json(
        &project.join(".opencode-gear.json"),
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
    let user = dir.join("user.json");
    write_json(
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
        Some(&dir.join("nope-user.json")),
        Some(&dir.join("nope-project.json")),
    );
    assert!(effective.applied.is_empty());
    assert_eq!(validate::validate(&effective), Vec::<String>::new());
}

#[test]
fn project_override_can_replace_a_prompt() {
    let (dir, home, project) = setup();
    let prompt_path = dir.join("custom-lead.md");
    fs::write(&prompt_path, "Custom lead prompt.\n").expect("write prompt");
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"prompts": {"lead": prompt_path.to_string_lossy()}}),
    );
    let effective = disk(&home, &project);
    let (_, body) = prompt::read_prompt(&effective, "lead").expect("read prompt");
    assert_eq!(body, "Custom lead prompt.");
}

#[test]
fn raw_opencode_override_is_merged_last() {
    let (_dir, home, project) = setup();
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"prompts": {"lead": {"append": "not-a-list"}}}),
    );
    let effective = disk(&home, &project);
    assert!(validate::validate(&effective)
        .iter()
        .any(|error| error.contains("append")));
}

#[test]
fn append_does_not_leak_into_consumer_prompts() {
    let (_dir, home, project) = setup();
    let policy = project.join("lead-policy.md");
    fs::write(&policy, "Lead-only clause.\n").expect("write");
    write_json(
        &project.join(".opencode-gear.json"),
        &json!({"prompts": {"lead": {"append": [policy.to_string_lossy()]}}}),
    );
    let effective = disk(&home, &project);
    let config = build::build_opencode_config(&effective, "low").expect("build");
    for role in model::role_specs(&effective.data).unwrap().keys() {
        let text = config["agent"][model::consumer_agent_id(role)]["prompt"]
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    write_json(
        &project.join(".opencode-gear.json"),
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
    for level in LEAD_LEVELS {
        let a = build::build_opencode_config(&embedded, level).expect("embedded build");
        let b = build::build_opencode_config(&disk_effective, level).expect("disk build");
        assert_eq!(a, b, "embedded and disk config differ at level {level}");
    }
}
