"""Tests for OpenCode Gear's configuration builder.

Run with:  python3 -m unittest discover -s tests -v
or:        make test
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def load_module():
    spec = importlib.util.spec_from_file_location("oc_config", ROOT / "bin" / "oc_config.py")
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


oc = load_module()


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, data: dict) -> None:
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


class GearTestCase(unittest.TestCase):
    """Base class that gives each test an isolated copy of the gear config."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)
        self.home = self.tmp / "gear"
        shutil.copytree(ROOT / "config", self.home / "config")
        self.project = self.tmp / "project"
        self.project.mkdir()

    def effective(self, user_config=None, project_config=None):
        effective, applied = oc.build_effective(
            self.home, self.project, user_config, project_config
        )
        effective["_home"] = str(self.home)
        effective["_cwd"] = str(self.project)
        return effective, applied

    def patch_project(self, data: dict) -> Path:
        path = self.project / ".opencode-gear.json"
        write_json(path, data)
        return path

    def patch_config(self, name: str, mutate) -> Path:
        path = self.home / "config" / name
        data = read_json(path)
        mutate(data)
        write_json(path, data)
        return path

    def errors(self, effective) -> list[str]:
        return oc.validate(effective, self.project)


class ThrottleTests(GearTestCase):
    def test_default_throttle_is_low(self) -> None:
        effective, _ = self.effective()
        self.assertEqual(oc.resolve_throttle(effective), "low")

    def test_exactly_three_levels(self) -> None:
        effective, _ = self.effective()
        self.assertEqual(set(effective["throttle"]["levels"]), {"low", "mid", "high"})

    def test_low_uses_sol_medium(self) -> None:
        effective, _ = self.effective()
        spec = effective["throttle"]["levels"]["low"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "openai/gpt-5.6-sol")
        self.assertEqual(spec["variant"], "medium")

    def test_mid_uses_sol_high(self) -> None:
        effective, _ = self.effective()
        spec = effective["throttle"]["levels"]["mid"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "openai/gpt-5.6-sol")
        self.assertEqual(spec["variant"], "high")

    def test_high_uses_astra(self) -> None:
        effective, _ = self.effective()
        spec = effective["throttle"]["levels"]["high"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "openai/gpt-6-astra")

    def test_lead_agent_per_level(self) -> None:
        for level in ("low", "mid", "high"):
            effective, _ = self.effective()
            config = oc.build_opencode_config(effective, level, self.project)
            self.assertEqual(config["default_agent"], f"lead-{level}")
            self.assertIn(f"lead-{level}", config["agent"])
            self.assertEqual(
                config["agent"][f"lead-{level}"]["model"],
                effective["throttle"]["levels"][level]
                and oc.model_full_id(effective, effective["throttle"]["levels"][level]["model"])[1],
            )

    def test_unknown_level_is_rejected(self) -> None:
        effective, _ = self.effective()
        with self.assertRaises(oc.ConfigError):
            oc.build_opencode_config(effective, "turbo", self.project)

    def test_cli_beats_environment(self) -> None:
        effective, _ = self.effective()
        with mock.patch.dict(os.environ, {"OC_GEAR_THROTTLE": "high"}):
            self.assertEqual(oc.resolve_throttle(effective), "high")
            self.assertEqual(oc.resolve_throttle(effective, "mid"), "mid")

    def test_environment_beats_default(self) -> None:
        effective, _ = self.effective()
        with mock.patch.dict(os.environ, {"OC_GEAR_THROTTLE": "high"}):
            self.assertEqual(oc.resolve_throttle(effective), "high")

    def test_project_override_can_change_default(self) -> None:
        self.patch_project({"throttle": {"default": "mid"}})
        effective, _ = self.effective()
        self.assertEqual(oc.resolve_throttle(effective), "mid")

    def test_throttle_does_not_change_consumer_routing(self) -> None:
        effective, _ = self.effective()
        baseline = {
            role: oc.model_full_id(effective, spec["model"])[1]
            for role, spec in oc.role_specs(effective).items()
        }
        for level in ("low", "mid", "high"):
            config = oc.build_opencode_config(effective, level, self.project)
            for role, full in baseline.items():
                agent = config["agent"][oc.consumer_agent_id(role)]
                self.assertEqual(agent["model"], full, f"{role} changed under throttle {level}")


class RoutingTests(GearTestCase):
    def test_explore_normal_routes_to_volcano_kimi_k2_7(self) -> None:
        effective, _ = self.effective()
        spec = oc.role_specs(effective)["explore"]
        self.assertEqual(
            oc.model_full_id(effective, spec["model"])[1], "volcengine-coding/kimi-k2.7-code"
        )

    def test_explore_deep_routes_to_volcano_kimi_k3(self) -> None:
        effective, _ = self.effective()
        spec = oc.role_specs(effective)["explore-deep"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "volcengine-coding/kimi-k3")

    def test_build_routes_to_opencode_go_deepseek(self) -> None:
        effective, _ = self.effective()
        spec = oc.role_specs(effective)["build"]
        self.assertEqual(
            oc.model_full_id(effective, spec["model"])[1], "opencode-go/deepseek-v4.1-flash"
        )

    def test_verify_routes_to_opencode_go_glm_flash(self) -> None:
        effective, _ = self.effective()
        spec = oc.role_specs(effective)["verify"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "opencode-go/glm-5.3-flash")

    def test_debug_routes_to_opencode_go_glm(self) -> None:
        effective, _ = self.effective()
        spec = oc.role_specs(effective)["debug"]
        self.assertEqual(oc.model_full_id(effective, spec["model"])[1], "opencode-go/glm-5.3")

    def test_provider_binding_is_deterministic(self) -> None:
        effective, _ = self.effective()
        for role, spec in oc.role_specs(effective).items():
            provider, _ = oc.model_full_id(effective, spec["model"])
            if role.startswith("explore"):
                self.assertEqual(provider, "volcengine-coding")
            else:
                self.assertEqual(provider, "opencode-go")

    def test_openai_is_not_a_consumer(self) -> None:
        effective, _ = self.effective()
        for spec in oc.role_specs(effective).values():
            provider, _ = oc.model_full_id(effective, spec["model"])
            self.assertNotEqual(provider, "openai")

    def test_consumer_agents_are_hidden_and_cannot_delegate(self) -> None:
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        for role in oc.role_specs(effective):
            agent = config["agent"][oc.consumer_agent_id(role)]
            self.assertTrue(agent["hidden"], role)
            self.assertEqual(agent["permission"]["task"], "deny", role)

    def test_lead_can_only_task_its_own_consumers(self) -> None:
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        task = config["agent"]["lead-low"]["permission"]["task"]
        self.assertEqual(task["*"], "deny")
        allowed = {k for k, v in task.items() if v == "allow"}
        self.assertEqual(
            allowed, {oc.consumer_agent_id(role) for role in oc.role_specs(effective)}
        )

    def test_enabled_providers_are_the_routing_providers(self) -> None:
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        self.assertEqual(
            set(config["enabled_providers"]),
            {"openai", "volcengine-coding", "opencode-go"},
        )

    def test_fallback_is_reported_and_validated(self) -> None:
        self.patch_project(
            {"routing": {"roles": {"build": {"model": "deepseek-v4.1-flash", "variant": "high",
                                             "fallback": [{"model": "glm-5.3", "variant": "high"}]}}}}
        )
        effective, _ = self.effective()
        self.assertEqual(self.errors(effective), [])
        block = oc.routing_block(effective, "low")
        self.assertIn("Configured fallbacks", block)


class ValidationTests(GearTestCase):
    def test_default_config_is_valid(self) -> None:
        effective, _ = self.effective()
        self.assertEqual(self.errors(effective), [])

    def test_missing_model_key_is_reported(self) -> None:
        self.patch_project({"routing": {"roles": {"build": {"model": "no-such-model"}}}})
        effective, _ = self.effective()
        errors = self.errors(effective)
        self.assertTrue(any("no-such-model" in e for e in errors), errors)

    def test_missing_provider_is_reported(self) -> None:
        def mutate(data):
            data["models"]["mystery"] = {"provider": "no-such-provider", "id": "mystery-1"}

        self.patch_config("models.json", mutate)
        self.patch_project({"routing": {"roles": {"build": {"model": "mystery"}}}})
        effective, _ = self.effective()
        errors = self.errors(effective)
        self.assertTrue(any("no-such-provider" in e for e in errors), errors)

    def test_unknown_variant_is_reported(self) -> None:
        self.patch_project({"routing": {"roles": {"build": {"model": "deepseek-v4.1-flash",
                                                           "variant": "impossible"}}}})
        effective, _ = self.effective()
        errors = self.errors(effective)
        self.assertTrue(any("impossible" in e for e in errors), errors)

    def test_valid_variant_is_accepted(self) -> None:
        self.patch_project({"routing": {"roles": {"build": {"model": "deepseek-v4.1-flash",
                                                           "variant": "max"}}}})
        effective, _ = self.effective()
        self.assertEqual(self.errors(effective), [])

    def test_unknown_throttle_level_model_is_reported(self) -> None:
        self.patch_config("throttle.json", lambda d: d["levels"]["mid"].update({"model": "ghost"}))
        effective, _ = self.effective()
        self.assertTrue(any("ghost" in e for e in self.errors(effective)))

    def test_missing_prompt_is_reported(self) -> None:
        (self.home / "config" / "prompts" / "docs.md").unlink()
        effective, _ = self.effective()
        self.assertTrue(any("docs" in e for e in self.errors(effective)))

    def test_require_valid_raises(self) -> None:
        self.patch_project({"routing": {"roles": {"build": {"model": "no-such-model"}}}})
        effective, _ = self.effective()
        with self.assertRaises(oc.ConfigError):
            oc.require_valid(effective, self.project)

    def test_missing_config_file_is_reported(self) -> None:
        (self.home / "config" / "models.json").unlink()
        with self.assertRaises(oc.ConfigError):
            oc.load_defaults(self.home)


class OverrideTests(GearTestCase):
    def test_project_override_swaps_a_model(self) -> None:
        self.patch_project({"models": {"models": {"glm-5.3": {"provider": "opencode-go",
                                                              "id": "glm-5.3",
                                                              "label": "GLM-5.3 custom"}}}})
        effective, _ = self.effective()
        self.assertEqual(oc.model_label(effective, "glm-5.3"), "GLM-5.3 custom")

    def test_project_override_beats_user_override(self) -> None:
        user = self.tmp / "user.json"
        write_json(user, {"routing": {"roles": {"build": {"model": "glm-5.3"}}}})
        self.patch_project({"routing": {"roles": {"build": {"model": "glm-5.3-flash"}}}})
        effective, _ = self.effective(user_config=str(user))
        self.assertEqual(oc.role_specs(effective)["build"]["model"], "glm-5.3-flash")

    def test_user_override_is_used_when_project_is_absent(self) -> None:
        user = self.tmp / "user.json"
        write_json(user, {"routing": {"roles": {"build": {"model": "glm-5.3"}}}})
        effective, applied = self.effective(user_config=str(user))
        self.assertEqual(oc.role_specs(effective)["build"]["model"], "glm-5.3")
        self.assertEqual([name for name, _ in applied], ["user"])

    def test_missing_override_files_are_ignored(self) -> None:
        effective, applied = self.effective(
            user_config=str(self.tmp / "nope-user.json"),
            project_config=str(self.tmp / "nope-project.json"),
        )
        self.assertEqual(applied, [])
        self.assertEqual(self.errors(effective), [])

    def test_project_override_can_replace_a_prompt(self) -> None:
        prompt = self.tmp / "custom-lead.md"
        prompt.write_text("Custom lead prompt.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": str(prompt)}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertEqual(body, "Custom lead prompt.")

    def test_raw_opencode_override_is_merged_last(self) -> None:
        self.patch_project({"opencode": {"username": "gearbox"}})
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        self.assertEqual(config["username"], "gearbox")

    def test_relative_prompt_path_resolves_against_project(self) -> None:
        (self.project / "lead.md").write_text("Relative lead prompt.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": "lead.md"}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertEqual(body, "Relative lead prompt.")


class PromptExtensionTests(GearTestCase):
    """Project policy must extend the gear prompt, never fork it."""

    def core_lead(self) -> str:
        return (self.home / "config" / "prompts" / "lead.md").read_text(encoding="utf-8")

    def test_append_keeps_core_prompt_and_adds_project_policy(self) -> None:
        policy = self.project / "lead-policy.md"
        policy.write_text("# Project policy\n\nNever touch production.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"append": [str(policy)]}}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertIn("You are the Lead in an OpenCode Gear multi-model setup.", body)
        self.assertIn("Never touch production.", body)
        self.assertIn(oc.PROMPT_APPEND_SEPARATOR.strip(), body)
        self.assertLess(body.index("You are the Lead"), body.index("Never touch production."))

    def test_append_only_uses_the_gear_default_not_a_replacement(self) -> None:
        policy = self.project / "policy.md"
        policy.write_text("Project-only clause.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"append": [str(policy)]}}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        # The gear prompt is still present verbatim at the front.
        self.assertTrue(body.startswith(self.core_lead().strip()))
        self.assertTrue(body.endswith("Project-only clause."))

    def test_append_accepts_inline_text(self) -> None:
        self.patch_project({"prompts": {"lead": {"append": [{"text": "Inline clause."}]}}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertIn("Inline clause.", body)

    def test_path_then_append_replaces_and_extends(self) -> None:
        replacement = self.project / "replacement.md"
        replacement.write_text("Replacement core.\n", encoding="utf-8")
        extra = self.project / "extra.md"
        extra.write_text("Extra policy.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"path": str(replacement), "append": [str(extra)]}}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertTrue(body.startswith("Replacement core."))
        self.assertTrue(body.endswith("Extra policy."))
        self.assertNotIn("You are the Lead in an OpenCode Gear", body)

    def test_append_accepts_relative_project_path(self) -> None:
        nested = self.project / "policy"
        nested.mkdir()
        (nested / "lead.md").write_text("Relative project policy.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"append": ["policy/lead.md"]}}})
        effective, _ = self.effective()
        _, body = oc.read_prompt(effective, "lead", self.project)
        self.assertTrue(body.endswith("Relative project policy."))

    def test_appended_policy_reaches_the_rendered_lead_agent(self) -> None:
        policy = self.project / "lead-policy.md"
        policy.write_text("Use `{{build}}` only for approved scope.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"append": [str(policy)]}}})
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        prompt = config["agent"]["lead-low"]["prompt"]
        self.assertIn("Use `ocg-build` only for approved scope.", prompt)

    def test_missing_appended_file_is_reported(self) -> None:
        self.patch_project({"prompts": {"lead": {"append": ["missing-policy.md"]}}})
        effective, _ = self.effective()
        self.assertTrue(any("lead" in e for e in self.errors(effective)), self.errors(effective))

    def test_append_must_be_a_list(self) -> None:
        self.patch_project({"prompts": {"lead": {"append": "not-a-list"}}})
        effective, _ = self.effective()
        self.assertTrue(any("append" in e for e in self.errors(effective)))

    def test_append_does_not_leak_into_consumer_prompts(self) -> None:
        policy = self.project / "lead-policy.md"
        policy.write_text("Lead-only clause.\n", encoding="utf-8")
        self.patch_project({"prompts": {"lead": {"append": [str(policy)]}}})
        effective, _ = self.effective()
        config = oc.build_opencode_config(effective, "low", self.project)
        for role in oc.role_specs(effective):
            self.assertNotIn("Lead-only clause.", config["agent"][oc.consumer_agent_id(role)]["prompt"])

    def test_core_lead_prompt_stays_project_agnostic(self) -> None:
        core = self.core_lead()
        for token in ("Zh" + "uju", "xiang" + "min", "chun" + "cheon", "/opt/" + "zhuju"):
            self.assertNotIn(token, core)
        self.assertNotIn("/home/", core)
        self.assertNotIn("/Users/", core)


class ProjectLayerTests(GearTestCase):
    def test_project_override_is_loaded_only_for_that_project(self) -> None:
        self.patch_project({"throttle": {"default": "high"}})
        here, applied_here = self.effective()
        other = self.tmp / "other-project"
        other.mkdir()
        elsewhere, applied_elsewhere = oc.build_effective(self.home, other)
        self.assertEqual(oc.resolve_throttle(here), "high")
        self.assertEqual(oc.resolve_throttle(elsewhere), "low")
        self.assertEqual([name for name, _ in applied_here], ["project"])
        self.assertEqual(applied_elsewhere, [])

    def test_rendered_lead_is_the_core_prompt_when_no_project_policy_exists(self) -> None:
        effective, applied = self.effective()
        self.assertEqual(applied, [])
        config = oc.build_opencode_config(effective, "low", self.project)
        core = (self.home / "config" / "prompts" / "lead.md").read_text(encoding="utf-8")
        # Only the template placeholders are substituted; no project text is injected.
        rendered = config["agent"]["lead-low"]["prompt"]
        self.assertIn("You are the Lead in an OpenCode Gear multi-model setup.", rendered)
        for line in core.splitlines():
            stripped = line.strip()
            if stripped and "{{" not in stripped and "|" not in stripped:
                self.assertIn(stripped, rendered)


class ObservabilityTests(GearTestCase):
    def test_trace_is_disabled_by_default(self) -> None:
        effective, _ = self.effective()
        self.assertIsNone(oc.trace_path(effective))
        self.assertIsNone(oc.record_event(effective, "launch", "low"))

    def test_trace_writes_local_jsonl_without_content(self) -> None:
        trace_file = self.tmp / "events.jsonl"
        self.patch_project({"observability": {"enabled": True, "path": str(trace_file)}})
        effective, _ = self.effective()
        self.assertEqual(oc.record_event(effective, "launch", "mid"), trace_file)
        record = json.loads(trace_file.read_text(encoding="utf-8").strip())
        self.assertEqual(record["event"], "launch")
        self.assertEqual(record["throttle"], "mid")
        self.assertEqual(record["default_agent"], "lead-mid")
        self.assertEqual(record["routing"]["build"], "opencode-go/deepseek-v4.1-flash")
        self.assertNotIn("prompt", json.dumps(record).lower())
        self.assertNotIn("source", json.dumps(record).lower())

    def test_trace_env_override_is_used(self) -> None:
        self.patch_project({"observability": {"enabled": True}})
        effective, _ = self.effective()
        trace_file = self.tmp / "env-events.jsonl"
        with mock.patch.dict(os.environ, {"OC_GEAR_TRACE": str(trace_file)}):
            self.assertEqual(oc.trace_path(effective), trace_file)


class RepositoryHygieneTests(unittest.TestCase):
    """The published tree must not leak secrets or a private environment."""

    text_suffixes = {".md", ".json", ".py", ".sh", ".yaml", ".yml", ".toml", ".txt", ".cfg", ".ini", ""}

    def text_files(self):
        skip_dirs = {".git", "__pycache__", "node_modules", ".venv"}
        for path in sorted(ROOT.rglob("*")):
            if not path.is_file():
                continue
            if any(part in skip_dirs for part in path.parts):
                continue
            if path.suffix.lower() in self.text_suffixes or path.name in {"LICENSE", ".gitignore"}:
                yield path

    def test_all_json_files_parse(self) -> None:
        for path in sorted(ROOT.rglob("*.json")):
            if ".git" in path.parts:
                continue
            with self.subTest(path=str(path)):
                json.loads(path.read_text(encoding="utf-8"))

    def test_no_private_project_tokens(self) -> None:
        tokens = [
            "Zh" + "uju",
            "Route" + "Lace",
            "Cai" + "bao",
            "xiang" + "min",
            "chun" + "cheon",
            "/opt/" + "zhuju",
            "zj-" + "builder",
            "zj-" + "explorer",
            "zj-" + "verifier",
            "zj-" + "docs",
            "forward_" + "to_gpt",
        ]
        for path in self.text_files():
            text = path.read_text(encoding="utf-8", errors="ignore")
            for token in tokens:
                with self.subTest(path=str(path.relative_to(ROOT)), token=token):
                    self.assertNotIn(token, text)

    def test_no_secrets_or_absolute_home_paths(self) -> None:
        patterns = [
            "AK" + "IA[0-9A-Z]{16}",
            "sk" + "-[A-Za-z0-9]{20,}",
            "gh" + "p_[A-Za-z0-9]{36}",
            "xox" + "[baprs]-[A-Za-z0-9-]+",
            "-----BEGIN " + "[A-Z ]*PRIVATE KEY-----",
            r"/home/" + r"[A-Za-z0-9._-]+/",
            r"/Users/" + r"[A-Za-z0-9._-]+/",
            r"[A-Za-z0-9._%+-]+" + "@" + r"[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
        ]
        compiled = [re.compile(p) for p in patterns]
        for path in self.text_files():
            text = path.read_text(encoding="utf-8", errors="ignore")
            for pattern in compiled:
                with self.subTest(path=str(path.relative_to(ROOT)), pattern=pattern.pattern):
                    self.assertIsNone(pattern.search(text))


if __name__ == "__main__":
    unittest.main()
