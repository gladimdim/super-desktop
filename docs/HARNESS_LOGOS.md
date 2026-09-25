# Harness product logos

Linux and Android bundle the same audited asset manifest,
[`assets/logos/harness-logos.json`](../assets/logos/harness-logos.json).
It maps stable harness IDs to source SVGs and themeable SVG templates, records
source URLs and file hashes, and never accepts a filename from a bridge.
[Attribution](../assets/logos/ATTRIBUTION.md) and
[upstream license notices](../assets/logos/LICENSES.md) accompany the assets.

## Implemented surfaces

- Linux: local/remote harness launcher bars, built-in launcher settings, terminal
  card headings, and iconified cards. Card and toolbar marks update on theme changes.
- Android: harness rows, primary/secondary terminal headings, create-harness and
  DUO pickers, home-screen harness widgets, and expanded bridge-toolbar notification.
  Marks render at device density and use a bounded bitmap cache.
- Names remain text for accessibility. Custom launchers retain their selected
  glyph. Unknown IDs are not mapped to another product's logo.
- Herder retains its old glyph: no verifiable SVG mark was found in its upstream
  repository. Shell uses the existing generic terminal SVG, not a product trademark.

There are 18 branded harness mappings plus Shell. Existing OpenAI, OpenCode and
Grok artwork is retained; Grok's standalone symbol is extracted from its sourced
wordmark for legibility. New SVGs replace the company substitutes for Claude and
Antigravity. Pi, OpenClaw, Hermes, Goose, Qwen, Crush, Aider, Kiro, Cursor, Reasonix,
Gemini and Kimi also have bundled marks. DeepSeek Harness (`dsh`, added 2026-09-25)
uses DeepSeek's whale mark from Lobe Icons (MIT), because neither `dsh` nor its
`dsh-tui` terminal UI ships a product mark of its own. T3 Code's mark was removed with its
launcher (2026-09-25): it runs a web server, not a terminal harness.

The default display now uses the active theme’s accent for visible strokes/fills
and its background for interior details. Theme templates retain path geometry and
mask colors while omitting color-blend filters and animation. This avoids tinting
boxed logos into solid squares. Both platforms render these variants as SVG,
including Antigravity. Original colored SVGs and its Android raster remain source
and fallback assets. Custom glyphs are unchanged.

Linux caches substituted SVGs by content hash and refreshes image sources during
theme reload. Android’s bitmap cache key includes ink and background colors as
well as file and pixel size; changing palettes cannot reuse the old colored image.
Android’s existing theme recreation/widget refresh redraws the marks.

## Updating assets

1. Find the identifying mark in the upstream site/repository or a documented
   brand-icon mirror; do not substitute unrelated company or similarly named logos.
2. Download and inspect it. Preserve artwork geometry/proportions and brand colors.
   Record source, original download hash and any transformation in the manifest.
3. Run `python3 scripts/theme-harness-logos.py` to derive palette templates,
   then `python3 scripts/sync-harness-logos.py` to mirror
   the bundle into the sibling Android repository. `--check` validates parity.
4. Render both themes and test actual Android output; successful XML parsing alone
   does not prove that a mask, document size, or filter renders correctly.
5. Update the Android feature inventory and this document if coverage changes.

## Verification on 2026-09-24

- Asset hash/parity validation and all four Rust brand tests pass.
- Android unit tests and debug APK/test APK builds pass. Shared signing certificate
  remains `86718AA95D3C79E347002387238EB125CAB57F204638AA180A076E64EB4E1C8D`.
- Connected-phone instrumentation rendered all 38 light/dark logo entries and
  tested requested sizing, name preservation and recycled custom-icon fallback.
  The generated contact sheet was visually inspected. It caught and drove fixes
  for intrinsic SVG sizing and Antigravity's unsupported alpha-mask/blur behavior.
- Latest pre-push Rust suite: 300 passed, five failed, six ignored. The
  `bridge::tests::test_port_taken_follows_the_listener` port-release assertion
  passed when rerun alone. The four remaining failures match earlier validation:
  `overlap_ghost::tests::an_outline_follows_its_card_after_a_resize`,
  `window::tests::slide_moves_a_card_inside_its_own_canvas`,
  `window::tests::toolbar_controls_stay_on_screen`, and
  `tmux::tests::test_resolve_command_ai_agent_arguments`.
  The first three fail mapped GTK geometry assertions on this display; the last
  expects Antigravity to be installed. `cargo test toolbar_` passed three of four
  checks; the mapped local toolbar window did not accept requested resize widths.
  Isolated Broadway/X11 attempts did not eliminate the geometry failure. No tests
  were weakened or disabled to hide these failures. The six existing default
  exclusions include the separately exercised isolated clipboard test.

Run the phone logo check with the installed debug/test APKs:

```sh
adb shell am instrument -w -e harnessLogosOnly true \
  com.omarchy.ailauncher.test/com.omarchy.ailauncher.TerminalRenderingInstrumentation
```

The default rendering instrumentation also includes the logo checks. No bridge is
opened and no harness input is sent by this check.

Theme validation: Rust tests check all palette substitutions and preserved mask
white. Connected-phone tests render every mark with both light/dark palettes and
verify that changing ink color returns a different, visibly recolored cached image.
