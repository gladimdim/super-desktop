# SUPER DESKTOP contributor instructions

When a change in this repository adds, changes, or removes a feature exposed to
the Android companion, update the maintained Android feature inventory in the
sibling `OmarchyAILauncher` repository at `designs/ANDROID_FEATURES.md` in the
same work. Document the user entry point, behavior, bridge dependency, and
meaningful limits. Keep planned features out of the implemented inventory.

The Android repository's own `AGENTS.md` has the full Android feature and build
instructions. Changes only to PC-to-PC or Linux-only behavior do not require an
Android feature inventory update.

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
