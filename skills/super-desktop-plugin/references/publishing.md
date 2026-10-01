# Publishing a plugin

One plugin = one public GitHub repository. Users install it by pasting
`owner/repo` (optionally `@vX.Y.Z`) into Settings → Plugins, from a registry,
or with `super-desktop plugin install owner/repo`.

## Repository layout

```
super-desktop-plugin.json   manifest (required, at the root)
README.md                   what it does, what it sends where, settings, permissions and why
LICENSE
AGENTS.md                   points coding agents at this skill (see below)
icons/                      SVG/PNG, ≤ 256 KiB each
main.py | main.mjs | …      process entry, plus sd_plugin.py if you use the Python SDK
renderer.wasm               a built renderer, committed or attached as a release asset
tests/                      scenarios for `super-desktop plugin test`
```

Ship runnable files. Installation does not build anything: scripts run as
they are, and binaries and `.wasm` files are either committed or attached to
the GitHub Release and listed under `assets` with their `sha256`. Keep the
source of any binary in the repository and a script that rebuilds it
(`build.sh`), so a reviewer can reproduce the hash.

## Releasing

1. Raise `version` in the manifest.
2. `super-desktop plugin validate . --json` and `super-desktop plugin test .`
   must pass.
3. Commit, then tag that commit `v<version>` (annotated) and push the tag.
4. Create a GitHub Release from the tag; attach assets if the manifest lists
   any, and check their `sha256` against the manifest.
5. In the release notes, list user-visible changes and **every change in
   permissions, shortcuts or sandbox**: users must approve those again.

Installs pin the commit of the tag. Never move or delete a published tag;
fix a mistake with a new version.

## Compatibility

- Keep `id`, setting keys, contribution ids and harness ids stable; users'
  settings, storage and shortcuts are keyed by them.
- Raise `engines.superDesktop` when you start using a newer host method; check
  `host.describe().methods` at run time for optional features instead.
- Read settings defensively: a value may be missing after an update.

## Registries

The official registry is the GitHub repository
[`gladimdim/super-desktop-plugins`](https://github.com/gladimdim/super-desktop-plugins),
whose `registry.json` lists plugins (normative schema:
`schemas/registry.schema.json`):

```json
{
  "registryVersion": 1,
  "plugins": [
    { "id": "flusher", "name": "Flusher", "description": "…", "repo": "gladimdim/super-desktop-flusher",
      "tags": ["git", "agents"], "author": "gladimdim", "minHost": "1.2.0" }
  ]
}
```

Open a pull request adding your entry, sorted by id. Reviewed versions get
`reviewedTag`/`reviewedCommit` and a badge in the app. A local registry is the
same file (or a folder of plugin folders) added in Settings → Plugins →
Sources.

## Help the next coding agent

Put an `AGENTS.md` (and a `CLAUDE.md` that says `@AGENTS.md`) in the plugin
repository:

```markdown
# Agent instructions

This is a SUPER DESKTOP plugin. Follow the `super-desktop-plugin` skill:
https://github.com/gladimdim/super-desktop/tree/master/skills/super-desktop-plugin
(install it into your agent's skills folder, or read SKILL.md there first).

- The manifest is `super-desktop-plugin.json`; validate with
  `super-desktop plugin validate . --json` after every change.
- Run `super-desktop plugin test .` before committing.
- Never print to stdout from plugin code; it is the protocol channel.
```

`super-desktop plugin new` writes these files and copies the skill into
`.agents/skills/` and `.claude/skills/` of the new repository.
