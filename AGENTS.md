# SUPER DESKTOP contributor instructions

When a change in this repository adds, changes, or removes a feature exposed to
the Android companion, update the maintained Android feature inventory in the
sibling `OmarchyAILauncher` repository at `designs/ANDROID_FEATURES.md` in the
same work. Document the user entry point, behavior, bridge dependency, and
meaningful limits. Keep planned features out of the implemented inventory.

The Android repository's own `AGENTS.md` has the full Android feature and build
instructions. Changes only to PC-to-PC or Linux-only behavior do not require an
Android feature inventory update.

## Bridge protocol compatibility (released PCs and released phones)

The phone bridge (`PROTOCOL.md` in the private repository, wire version 3)
connects SUPER DESKTOP on a PC to the Android app. The two are updated
independently: PCs through Settings → Updates or the install command, phones
through Google Play. Any released desktop build must keep working with any
released Android build, in both directions. Never assume both sides update
together.

- **Before changing the bridge in any way, ask the user and wait for a yes.**
  That covers every endpoint, message, field, header, pairing, auth or
  discovery change, additive or not. Say what changes, why, and how old phones
  and old PCs keep working. Do not start the change on a maybe.
- Change the bridge only additively: new optional fields, endpoints and
  headers. Never remove, rename or retype a field, change a unit or meaning,
  tighten validation, or reuse an old name for new behavior. A breaking wire
  change needs the user's agreement first.
- An old phone on this new PC must keep working unchanged, and a new phone on
  an old PC must degrade cleanly. Keep every existing response shape, status
  code and route, and keep ignoring unknown request fields.
- A change to the bridge is made together with the matching Android work in
  `../OmarchyAILauncher` and published together: publish the desktop release
  first, then the app, after testing this build against the latest released
  Android app and the new app against this build. Do not release the bridge
  side alone unless the change is additive and verified against the released app.
- State the minimum version each side needs in the commit message and in the
  notes (`designs/ANDROID_FEATURES.md` and `PROTOCOL.md` in the private
  repository).
- Keep the compatibility paths and their tests; remove one only after the user
  confirms that no supported release still uses it.

## Releases (git tags)

A release is an annotated git tag `vX.Y.Z` on the commit whose `Cargo.toml`
version is X.Y.Z, plus a GitHub Release. People use it to install an exact
version (`SUPER_DESKTOP_VERSION=vX.Y.Z` with `install.sh`, see the README's
"Installing a specific version"), for example to match an older phone app.
Make one only when the user says to release; never tag on your own. The
version changes only for a release, so every version is a release.

When the user asks for a release:

1. Start from a clean `master` that is pushed, with the tests passing. Read the
   last tag (`git tag --sort=-v:refname | head -1`) and the current version,
   and get the new version from the user.
2. Write the changelog from `git log <last tag>..HEAD` in plain language, in
   groups: what users see, bridge changes, fixes. Public rules apply: shipped
   behavior only, no links to the private notes, no unreleased plans.
3. Work out the protocol version (`PROTOCOL.md` in the private repository) and
   the phone app versions it works with: the oldest and the newest Android app,
   from `../OmarchyAILauncher/designs/ANDROID_FEATURES.md`, the app's
   `versionName` and Play release history, and a real check with each
   supported app build where you can. Say plainly what was tested and what was
   not. If the release changes the bridge, name the minimum version each side
   needs.
4. Show the user the changelog and those versions, and publish only after they
   agree.
5. Raise the version in `Cargo.toml` and `Cargo.lock` in one release commit
   ("Release X.Y.Z") and push it. Then tag and publish that commit:
   `git tag -a vX.Y.Z -m "SUPER DESKTOP X.Y.Z" <commit>`,
   `git push origin vX.Y.Z`, then `gh release create vX.Y.Z --title
   "SUPER DESKTOP X.Y.Z" --notes-file <notes>`. The notes start with three
   lines: Protocol, Android app (oldest to newest), and Install
   (`SUPER_DESKTOP_VERSION=vX.Y.Z` command), then the changelog. Leave the
   Android line out, and name no app version anywhere, until the user says the
   Android app is public.
6. Add a row to the README's compatibility table (a normal commit; the release
   tag stays on the release commit). Then check the command in a scratch
   clone (`git clone --branch vX.Y.Z`), never on the user's own install, and
   open the Release page.
7. Never move, delete or re-tag a published tag. A mistake is fixed with a new
   release. Tell the user to release the matching Android build in the same
   window when the bridge changed.

## Docs and plans live in the private OmarchyAILauncher repository

This repository is public. Its plans, design notes, protocol notes,
performance measurements and implementation write-ups are kept secret in the
private sibling repository, at `../OmarchyAILauncher/desktop-docs/` (index:
`desktop-docs/README.md`). It holds the PC-to-PC plan and protocol, harness
integrations and logos, completion notifications, file previews, phone prompt attachments,
mobile terminal output and Linux performance notes.

- Read the relevant `desktop-docs/` file before changing that area, and update
  it in the same work when behavior, limits or plans change. Commit and push
  that update in the OmarchyAILauncher repository.
- Write every new plan, design document, investigation or measurement there,
  and add it to the index. Never add one to this repository, including under
  `docs/`, which is the public website.
- Public files (README, SECURITY.md, the website, code comments, test
  docstrings, commit messages) must not link to or quote those notes, or
  describe unreleased plans. Describe shipped, user-facing behavior only.
- This repository's Markdown is limited to README.md, SECURITY.md, these
  instructions, `assets/logos/` attribution and licenses,
  `docs/screenshots/README.md`, and the plugin skills under `skills/` (see
  "Plugin API and skills").
- If `../OmarchyAILauncher` is not checked out, ask for it instead of writing
  the notes here.

## Plugin API and skills

`skills/` holds Agent Skills (a `SKILL.md` per folder) that teach coding agents
to write and review SUPER DESKTOP plugins: `super-desktop-plugin` (authoring:
references, an SDK, examples) and `super-desktop-plugin-review` (the review
checklist). Plugin authors copy them into their own repositories, and other
LLMs follow them, so they are the public contract of the plugin API.

- The schemas in `skills/super-desktop-plugin/schemas/` are normative: the
  manifest (`manifest.schema.json`), the JSON-RPC methods
  (`host-api.openrpc.json`) and the view nodes (`ui.schema.json`). The
  renderer byte layout is in `references/renderer-abi.md`. The host code must
  match them, not the other way round.
- Any change to the plugin host (manifest fields, permissions, contribution
  points, host methods or notifications, errors, limits, the renderer ABI,
  the `super-desktop plugin` commands) updates, in the same commit: the
  schemas, the affected `references/` file, `SKILL.md` (rules, limits,
  tables), the review checklist when a rule changes, and the examples.
- Plugin API 1 only grows, like the bridge: new optional fields, methods and
  contribution points. Never remove, rename or retype anything, tighten
  validation, or change a limit downward within API 1. A breaking change is
  API 2 and needs the user's agreement first.
- Keep the examples working: `skills/super-desktop-plugin/examples/center-magnify/build.sh`
  must pass, and each example manifest must validate against the schema.
  Keep each example's `sd_plugin.py` identical to `sdk/python/sd_plugin.py`.
- Every host error carries `data.hint` and `data.docs` (a `references/` file
  and section) so an agent can fix its own mistake. A new error needs both.
- The skills describe the API contract only: no roadmap, no unreleased plans,
  no links to the private notes. The plan lives in
  `../OmarchyAILauncher/desktop-docs/PLUGINS_PLAN.md`.
- Plugins never change the bridge or the PC-to-PC protocol (see the plan). A
  skill must never suggest otherwise.

## The version changes only for a release

Do not raise the version in `Cargo.toml` and `Cargo.lock` in ordinary commits.
It changes only when the user tells you to release a new version, and then to
the version they name (ask which if they did not): a release commit edits both
files together, and that commit is tagged (see "Releases (git tags)"). Settings
→ Updates offers an update when the version in Cargo.toml on GitHub is newer than
the running build, so it notices a release when its version bump reaches
`master`. The checked-in `.githooks/pre-commit` never changes a version; it
refuses a commit whose `Cargo.toml` and `Cargo.lock` versions differ. Enable it
once per clone with `git config core.hooksPath .githooks`, and do not bypass it
(`--no-verify`). A rebase or merge conflict in the version line resolves to the
higher version.

## Responsive toolbar invariant

The local and remote top bars must fit the current display's allocated logical
width, including after output, scale, and window-size changes. Never size the
toolbar from physical pixels, a minimum desktop resolution, saved card extents,
or an inactive workspace's preferred size.

Arrange, Settings, and Hide must remain visible, right-aligned within the bar's
padding, and clickable. Keep these actions outside scrolling containers. Let
launchers and folder controls shrink or scroll, and hide optional brand/shortcut
labels on narrow displays before sacrificing the actions. Off-screen, restored,
dragged, or animating cards must not enlarge the overlay window.

Any change to toolbar layout, sizing, contents, or workspace containers must
preserve and run `cargo test toolbar_`. Regression coverage must include all
three toolbar sizes, narrow and wide logical widths, repeated shrinking and
growing, empty and overflowing launcher lists, long folder paths, off-screen
cards, and a mapped window with hit tests for every right-side action. Exercise
the production sizing callback; a manually allocated standalone widget alone
is not sufficient. Run the full test suite before rebuilding the installed app,
and report any unrelated failures or excluded tests explicitly.

## Card titles (regressed three times)

A card's title, and the phone's `lastPrompt`, must only ever show a prompt the
user submitted to that card's own harness. Never derive it from terminal
output, assistant text, tool results, or turns the harness injects itself.
Claude Code, for example, writes background-task results (`<task-notification>`),
slash-command echoes, `!` shell input/output, reminders and auto-continuations
as "user" turns, and sends them to the `UserPromptSubmit` hook.

- Every prompt stored in or read from harness metadata must pass
  `harness_record::is_user_prompt`: in `apply`, `apply_claude`, the Claude
  transcript reader, and `harness_metadata::inspect_option` (the single read
  path for desktop cards, the phone list and the phone terminal header). A new
  prompt source must use the same check; do not bypass it.
- In Claude transcripts, skip user records with `promptSource: "system"` or an
  `origin.kind` of `task-notification`/`auto-continuation`. When Claude Code
  adds a new injected kind, extend the filter and the tests together.
- The `UserPromptSubmit` hook input carries only the text, with no origin. Some
  injected turns are plain prose (the usage-limit "Your claude.ai usage limit
  has reset. Continue…" auto-continuation), so the hook's prompt is also
  vetoed when the transcript records the same text as injected, and Stop
  repairs a stored prompt the transcript marks as injected (its record can
  land after the hook). Do not remove either step.
- An injected turn still counts as a turn for status and completion tracking;
  it only must not replace the prompt.
- Any change to prompt/title capture, harness hooks or adapters must preserve
  and run `cargo test card_title_`. Those tests must fail if the filter is
  removed; add a case for every new injected-turn shape you see in the wild.

## GTK tests never use the user's desktop

Tests must not open windows on the user's screen. Every GTK test runs its
assertions in a child process through `gtk_test::run_in_child_process`, which
starts a private, invisible Broadway display (`gtk4-broadwayd`, one per child,
web viewer bound to 127.0.0.1) and strips `WAYLAND_DISPLAY`, `DISPLAY` and the
Hyprland variables from the child. Without `gtk4-broadwayd` the test is skipped;
it never falls back to the desktop. Do not call `gtk4::init()` outside such a
child, and do not launch GTK probes, screenshots (`grim`) or input injection
(`wtype`) against the real session.

Broadway's screen is fixed at 1024×768, so tests that map larger windows or
need exact window geometry use `run_in_child_process_needing_large_screen` and
are skipped by default (currently the toolbar mapped-window hit tests, the
remote pan/zoom drag, the slide and the outline-resize tests). Run them only
with the user's agreement via `SD_GTK_TESTS_ON_DESKTOP=1`, or once a hidden
large virtual screen (Xvfb) is wired in. Report these skips explicitly. The same
rule covers scripts: `tests/two_pc_matrix.py` runs its GUI phase on Broadway.
