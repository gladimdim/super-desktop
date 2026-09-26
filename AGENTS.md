# SUPER DESKTOP contributor instructions

When a change in this repository adds, changes, or removes a feature exposed to
the Android companion, update the maintained Android feature inventory in the
sibling `OmarchyAILauncher` repository at `designs/ANDROID_FEATURES.md` in the
same work. Document the user entry point, behavior, bridge dependency, and
meaningful limits. Keep planned features out of the implemented inventory.

The Android repository's own `AGENTS.md` has the full Android feature and build
instructions. Changes only to PC-to-PC or Linux-only behavior do not require an
Android feature inventory update.

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
  instructions, `assets/logos/` attribution and licenses, and
  `docs/screenshots/README.md`.
- If `../OmarchyAILauncher` is not checked out, ask for it instead of writing
  the notes here.

## Every commit raises the version

Settings → Updates offers an update when the version in Cargo.toml on GitHub
is newer than the running build, so every commit raises it. The checked-in
`.githooks/pre-commit` raises the patch version in `Cargo.toml` and
`Cargo.lock` on each commit, touching only the version line. Enable it once
per clone with `git config core.hooksPath .githooks`, and do not bypass it
(`--no-verify`). Raise the minor or major version by editing `Cargo.toml` in
the commit itself; the hook keeps a raised version and syncs `Cargo.lock`. A
rebase or merge conflict in the version line resolves to the higher version;
the next commit raises it again.

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
