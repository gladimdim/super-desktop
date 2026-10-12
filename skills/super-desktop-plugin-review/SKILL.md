---
name: super-desktop-plugin-review
description: >
  Review a SUPER DESKTOP plugin repository for compliance, safety and quality before
  it is released, installed, or added to a plugin registry. Checks the manifest and
  permissions, sandbox, the JSON-RPC behaviour, deactivation cleanliness, prompt and
  title rules, LLM data handling, destructive actions, renderer ABI and release
  hygiene. Use when asked to review, audit, vet or approve a SUPER DESKTOP plugin,
  a registry pull request, or before tagging a plugin release.
---

# Reviewing a SUPER DESKTOP plugin

The standard is the `super-desktop-plugin` skill (its `SKILL.md` hard rules,
`references/` and `schemas/`). Read its `SKILL.md` first. This skill is the
checklist and the report format.

Review the code at a specific commit (the tag being released or listed), not a
branch. Read every file that runs: the manifest, process code, scripts it
calls, hook files for harnesses, and the source of every binary or `.wasm`.

## 1. Automatic checks

Run these and include their output in the report:

```sh
super-desktop plugin validate <dir> --json
super-desktop plugin test <dir>
```

For a renderer, also run its native tests and rebuild the `.wasm` from source;
the `sha256` must match the committed file or the manifest's `assets` entry.
A binary that cannot be rebuilt from the repository is a blocker.

## 2. Manual checklist

Mark each item **pass**, **fail** (with file:line) or **n/a**.

**Manifest and permissions**
- [ ] Every permission is used; nothing broader would do (e.g. `cards.read`
      instead of `terminal.read` when only folders are needed).
- [ ] `terminal.write`, `"sandbox": "none"`, `network: true` and wide `write`
      paths are justified in the README.
- [ ] Shortcuts do not take common combinations without reason; the README
      lists them.
- [ ] `engines.superDesktop` covers every method used.

**Protocol behaviour**
- [ ] Nothing but JSON-RPC is written to stdout (search for `print`,
      `console.log`, subprocesses inheriting stdout).
- [ ] `activate` and `deactivate` return quickly; slow work is on threads.
- [ ] Incoming messages are handled off the reader loop; no deadlock when a
      handler calls the host.
- [ ] Unknown fields and methods are tolerated; errors use `data.hint`.

**Returns to normal**
- [ ] No writes to `~/.config/super-desktop`, `~/.config/hypr`, a harness's
      config, systemd units, crontabs, shell profiles, or autostart files.
      Harness config goes through `harness.writeConfig`.
- [ ] No background processes that escape the process group (`setsid`,
      `nohup`, `disown`, double fork, `systemd-run`).
- [ ] `plugin test` passes its deactivation check; turning the plugin off and
      on twice by hand leaves the desktop as it was.

**User data and actions**
- [ ] Every action that commits, pushes, deletes, closes, sends or types into
      a terminal follows an explicit user action, and large ones show a
      preview/confirmation first.
- [ ] No `--force`, `reset --hard`, `clean -fdx`, `rm -rf` on user paths, or
      silent overwrites.
- [ ] LLM prompts contain only what the task needs, exclude secrets, are
      size-capped, and the README says what is sent.
- [ ] No network calls other than the documented ones; no telemetry without
      an opt-in setting.
- [ ] Secrets are `secret` settings, never logged.

**Titles and harnesses**
- [ ] Harness `prompt` events come only from the "user submitted" hook; every
      injected-turn prefix is listed in `prompts.injectedPrefixes`.
- [ ] `title.set` text is derived from the vetted prompt and the plugin's own
      data, not from terminal output that could carry injected instructions
      into the title.
- [ ] No attempt to reach the phone bridge, other PCs, or SUPER DESKTOP's
      sockets.

**Renderer**
- [ ] Exports exactly `memory`, `sd_abi_version`, `sd_input`, `sd_output`,
      `sd_present`; imports nothing but `env.sd_log`.
- [ ] Output card count equals input; numbers finite; dragged card follows the
      pointer; `animating` goes to 0 when settled.
- [ ] Per-frame work is linear (or n log n) in the card count; 128 cards stay
      within budget in `plugin test` (its `renderer` conformance run passes).
- [ ] Every `renderer.params` value is range-checked in the renderer, with a
      default for a missing or out-of-range value; the input buffer holds
      128 cards and 16 params.
- [ ] The first frame (`dt` 0) animates instead of jumping; the effect can be
      turned off without turning the plugin off (`renderer.toggle`).

**Release**
- [ ] Tag `v<version>` matches the manifest; release notes list permission,
      shortcut and sandbox changes.
- [ ] README: what it does, permissions and why, what is sent where, settings,
      how to uninstall.
- [ ] `AGENTS.md` points coding agents at the `super-desktop-plugin` skill.

## 3. Report

Write the report in this shape:

```
Plugin: <id> <version> @ <commit>
Verdict: approve | approve with changes | reject

Automatic checks
- validate: <ok / errors>
- test: <ok / failures>
- wasm reproducible: <yes / no / n/a>

Blockers
1. <file:line> <what> — <why it matters> — <fix>

Changes requested
1. ...

Notes
- ...
```

Blockers are any failed item under "Returns to normal", "User data and
actions", or "Titles and harnesses", any unreproducible binary, and any
failing automatic check. Be specific: quote the line, name the fix.
