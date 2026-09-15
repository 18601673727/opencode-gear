# Migrating from an older Gear / profile setup

This document is for people who already had a working, private multi-model
OpenCode setup and want to keep the workflow while adopting the split between
**Throttle** and the **Consumer Router**.

## What usually gets conflated

Older setups tend to use one concept for two jobs. Typical symptoms:

- "Gear" (or "profile", or "mode") names a bundle: `gear-low` means *this Lead*
  **and** *this builder* **and** *this verifier* at the same time.
- Switching the Lead tier also switches the builder, so a cheap Lead
  accidentally downgrades your implementation model.
- Subagents are duplicated per tier (`builder-low`, `builder-mid`,
  `builder-high`) even though the builder model never changes.
- A "mode" restricts providers and is easy to confuse with the Lead tier.

## The new truth

```text
Throttle        = OpenAI Lead tier only          low / mid / high
Consumer Router = delegated execution routing    EXPLORE / BUILD / VERIFY / DEBUG
```

## Step-by-step

1. **Inventory your current setup.** Write down, for each set of options you
   actually use: which Lead model, which builder, which explorer, which
   verifier.

2. **Decide the Lead axis.** Keep at most three Lead tiers. Map old names onto
   `low` / `mid` / `high` and put them in `config/throttle.json` (or your user
   override). If your old setup had only one Lead, keep one level and reuse it.

3. **Decide the consumer axis, once.** Pick exactly one model per role:
   explore, build, verify, debug. These no longer vary by Lead tier. Put them
   in `config/routing.json` (or your project override).

4. **Collapse duplicated subagents.** Delete per-tier duplicates
   (`builder-low`, `builder-mid`, ...). OpenCode Gear generates one consumer
   agent per role and shares it across all Lead levels.

5. **Move prompts to one place per role.** A single prompt per role lives in
   `config/prompts/`. Project-specific instructions belong in the repository's
   own agent instructions, not in the gear.

6. **Move project-specific routing into a project override.** Anything that
   named a private project, domain or module belongs in
   `<project>/.opencode-gear.json`, never in the shared core.

7. **Replace mode switching.**
   - Interactive, per session: `oc --throttle high`, or `Tab` in the TUI.
   - Persistent default: `oc throttle mid`.
   - Provider-scope restrictions (the old "mode" idea): express them as an
     explicit `models` + `routing` override for that project.

8. **Validate.** `oc validate` must pass, and `oc --dry-run` must show the
   agents you expect. Then run one real task.

## Name mapping cheat-sheet

| Old | New |
| --- | --- |
| Gear = Lead tier | Throttle level |
| Gear = full bundle | gone; consumers are throttle-independent |
| `builder-low/mid/high` | `ocg-build` |
| `explorer-*` | `ocg-explore`, `ocg-explore-deep` |
| `verifier-*` | `ocg-verify`, `ocg-debug` |
| `oc use <mode>` | `oc --throttle <level>` / override file |
| Lead-Low startup | default `low`; change with `oc throttle` |

## Keeping the old CLI alive during migration

If you have muscle memory for an old `oc use ...` command, keep the old script
on `PATH` under a different name (for example `oc-old`) while you migrate, then
delete it. Do not keep two scripts named `oc`; the second one on `PATH` wins and
the failure is confusing.

The same applies to any unrelated tool that already owns the name `oc`. `oc` is
the OpenCode Gear CLI; rename the other tool, or put the gear's `bin/`
directory earlier on `PATH`.

## What intentionally did not carry over

- Whole-bundle "Gear" switching. It is the thing this project exists to remove.
- Automatic escalation. Escalation is prompt policy in the Lead prompt; there
  is no scheduler, and pretending otherwise would be misleading.
- Provider fail-over that silently changes routing. Fallbacks exist but must be
  declared per role and are always visible.
