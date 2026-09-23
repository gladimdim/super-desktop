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
