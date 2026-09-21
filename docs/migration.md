# Migrating from an older Gear / profile setup

This document is for people who already had a working, private multi-model
OpenCode setup and want to keep the workflow while adopting the split between
**Throttle** and the **Worker Router**.

## What usually gets conflated

Older setups tend to use one concept for two jobs. Typical symptoms:

- "Gear" (or "profile", or "mode") names a bundle: `gear-low` means *this Lead*
  **and** *this builder* **and** *this verifier* at the same time.
- Switching the Execution Tier also switches the builder, so a cheap Lead
  accidentally downgrades your implementation model.
- Subagents are duplicated per tier (`builder-low`, `builder-mid`,
  `builder-high`) even though the builder model never changes.
- A "mode" restricts providers and is easy to confuse with the Execution Tier.

## The new truth

```text
Throttle        = Execution Tier only            low / mid / high
Worker Router   = delegated execution routing    EXPLORE / BUILD / VERIFY / DEBUG
```

## JSON files are now YAML

OpenCode Gear 0.3 reads YAML only. The canonical files are:

```text
~/.config/opencode-gear/config.yaml        (user)
<project>/.opencode-gear.yaml              (project)
config/*.yaml                              (shipped registries)
```

If a legacy `config.json` or `.opencode-gear.json` still exists, `ocg` stops
with an error that names it; it is never read, migrated or merged. Convert the
contents to YAML and rename the file. Because JSON is a subset of YAML, the
easiest first step is to rename the file and then reformat it (for example
`ocg throttle mid` rewrites the user config in YAML). Delete the old `.json`
file once the `.yaml` file is in place.

Use `ocg init` to create a minimal, comment-only project `.opencode-gear.yaml`
that passes `ocg validate`; it refuses when a stale `.opencode-gear.json`
exists and never overwrites an existing YAML file.

## Step-by-step

1. **Inventory your current setup.** Write down, for each set of options you
   actually use: which Lead model, which builder, which explorer, which
   verifier.

2. **Decide the Lead axis.** Keep at most three Execution Tiers. Map old names onto
   `low` / `mid` / `high` and put them in `config/throttle.yaml` (or your user
   override). If your old setup had only one Lead, keep one level and reuse it.

3. **Decide the worker axis, once.** Pick exactly one model per role:
   explore, build, verify, debug. These no longer vary by Execution Tier. Put them
   in `config/routing.yaml` (or your project override).

4. **Collapse duplicated subagents.** Delete per-tier duplicates
   (`builder-low`, `builder-mid`, ...). OpenCode Gear generates one worker
   agent per role and shares it across all Lead levels.

5. **Move prompts to one place per role.** A single prompt per role lives in
   `config/prompts/`. Project-specific instructions belong in the repository's
   own agent instructions, not in the gear.

6. **Move project-specific routing into a project override.** Anything that
   named a private project, domain or module belongs in
   `<project>/.opencode-gear.yaml`, never in the shared core.

7. **Replace mode switching.**
   - Interactive, per session: `ocg high`, `ocg --throttle high`, or `Tab` in
     the TUI.
   - Persistent default: `ocg throttle mid`.
   - Provider-scope restrictions (the old "mode" idea): express them as an
     explicit `models` + `routing` override for that project.

8. **Validate.** `ocg validate` must pass, and `ocg --dry-run` must show the
   agents you expect. Then run one real task.

## Name mapping cheat-sheet

| Old | New |
| --- | --- |
| Gear = Execution Tier | Throttle level |
| Gear = full bundle | gone; workers are throttle-independent |
| `builder-low/mid/high` | `ocg-build` |
| `explorer-*` | `ocg-explore`, `ocg-explore-deep` |
| `verifier-*` | `ocg-verify`, `ocg-debug` |
| `oc use <mode>` | `ocg <level>` / `ocg --throttle <level>` / override file |
| Lead-Low startup | default `low`; change with `ocg throttle` |

## Environment variables

The canonical prefix is `OPENCODE_GEAR_*`. The older `OC_GEAR_*` names still
work, so an existing shell profile does not break. Prefer the new names in new
scripts; `OC_GEAR_OPENCODE_BIN` is explicitly preserved.

## Keeping an old CLI alive during migration

`ocg` does not claim the name `oc`. If you have an older `oc` script, it can
stay on `PATH` while you migrate; invoke the gear as `ocg` during the
transition, then remove the old script when you are done.
