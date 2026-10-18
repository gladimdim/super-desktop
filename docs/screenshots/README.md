# Live workspace screenshots

Captured from the author's running Omarchy desktop for the
SUPER DESKTOP website. These are actual application captures, not mockups.

- `live-desktop.webp`: 3440 × 1440 fullscreen capture with the overlay visible.
- `live-terminals.webp`: 1715 × 815 region capture showing Codex and Claude Code.
- `harness-bar.webp`: 680 × 64 region of the top bar's harness launchers.
- `pc-selector.webp`: 316 × 214 region with the This PC menu open.
- `folder-picker.webp`: 640 × 290 region with the working-directory dropdown open.

The three region captures were cropped from full-screen `grim` captures at
1× scale and encoded as WebP at quality 92.

The captures were encoded as WebP at quality 88 without resizing. Full-size
images are linked from the page; the terminal detail expands inline. Update
HTML dimensions, alt text, and captions when replacing them.

## README images

- `readme-overlay.webp`: 1920 × 1080 fullscreen capture of the overlay in the
  Tokyo Night theme, above Chrome showing the GitHub repository.
- `readme-themes.webp`: 1944 × 1104 grid of the same workspace in Catppuccin
  Latte, Rose Pine, Everforest and Gruvbox, each scaled to 960 × 540 and
  labeled with the theme name (Noto Sans Bold).
- `readme-toolbar.webp`: the top 1920 × 58 strip of the Tokyo Night capture.

Captured on 2026-10-03 with `grim` on the author's 1920 × 1080 laptop display
(scale 1) while the overlay showed real Claude Code and Codex sessions,
switching themes with `omarchy-theme-set` between captures. WebP at quality
88 (92 for the toolbar). The README also embeds `desktop-toggle.gif`.

## Workspace reveal demo

- `desktop-toggle.gif`: a 7.8-second, 1440 × 602 loop at 15 fps. Chrome shows
  the website, SUPER + SHIFT + Q brings the live overlay into view with the
  caption "Access all your harnesses with one button press.", the overlay stays
  open for three seconds, then the shortcut hides it again under "Once finished
  → hide it again and keep working".
- `desktop-toggle-poster.webp`: the first frame, shown to visitors who prefer
  reduced motion and when the demo is paused.

Cut from an Omarchy screen recording (3440 × 1440, 60 fps) with FFmpeg: two
segments joined to hold the overlay for three seconds, captions and keycap
badges rendered with ImageMagick/Pango (Noto Sans Bold) and overlaid, then
encoded with a shared GIF palette. The recording used a custom binding; the
badge shows the default product shortcut, Super + Shift + Q. The GIF autoplays
and loops; a Pause/Play button stops it, and reduced-motion visitors start on
the still.

## iOS companion (iPhone Duo)

- `ios-duo-split.png`: DUO mode, the harness list beside two live Codex terminals.
- `ios-duo-harnesses.png`: the harness list with the foldable placeholder detail.
- `ios-duo-terminal.png`: the harness list beside one open terminal.

Captured on 2026-09-25 from the in-development iOS app (`super-desktop-ios`) on
the unfolded inner display (2853 × 2007) of the Xcode "iPhone Duo" simulator,
driven by its `ParityTour/testWideTour` UI test against the repository's mock
bridge (`scripts/mock-bridge/`), so the harnesses and output are sample data.
Each screenshot was clipped with the device type's framebuffer mask, placed in a
drawn bezel modeled on the Simulator's `phone15` chrome with a soft shadow on a
transparent background, then scaled to 1600 × 1175 PNG.

## Connect your phone guide (`connect/`)

Screenshots for `connect.html`, taken on 2026-10-03 during a real pairing of an
Android phone (the released SUPER DESKTOP app) with SUPER DESKTOP on Linux.

- `connections.webp`, `add-device.webp`, `pair-phone.webp`,
  `request.webp`, `approved.webp`: the real Settings card and connection
  request panel, drawn on a private Broadway display (Tokyo Night theme) by the
  opt-in tests `harness_settings::tests::pairing_screenshots` and
  `pairing_request_ui::tests::pairing_request_screenshots`, against a
  disposable bridge with its own state directory.
- `phone-add.webp`, `phone-confirm.webp`, `phone-request.webp`, `phone-code.webp`,
  `phone-connected.webp`: `adb exec-out screencap` from a 1280 × 2772 phone,
  cropped below the status bar to 1280 × 1800 and scaled to 640 × 900.

The phone reached the disposable bridge through `adb reverse`, then over
Tailscale. IP addresses, and the names of other saved computers, were blurred
after OCR (tesseract) located them. The verification code is the one that
pairing really used; the invitation in the QR code has expired and its bridge
no longer exists. All images are WebP at quality 90–92.

## Feature clips (`../media/`)

Short muted loops for the website's feature sections, cut from
`../video/super-desktop-trailer.mp4` (real footage, see `../video/README.md`)
with FFmpeg: scaled to 1280 px wide, 30 fps, H.264 CRF 25, `+faststart`, no
audio. Each has a WebP poster taken from a frame inside it.

- `reveal.mp4` (0.0–4.0 s): the shortcut and the overlay sliding in.
- `prompt.mp4` (4.0–15.0 s): a prompt typed into a live Claude Code session.
- `phone.mp4` (17.2–24.8 s): the Android app on an unfolded Galaxy Z Fold 7.
- `remote.mp4` (24.8–38.3 s): opening a paired PC and typing into its session.

The captions in the clips come from the trailer. The interactive demos on the
page are a simulation in `../assets/` with sample data, labeled as such.

## Plugin pictures (`../media/plugin-*.webp`)

The "Real plugins" tab of the Plugins section. Each is one plugin surface
drawn off screen by SUPER DESKTOP 1.6.0's plugin host with `super-desktop
plugin snapshot --scale 2`, on 2026-10-14 in the Tokyo Night theme, from the
example plugins in `skills/super-desktop-plugin/examples/` copied to a
scratch plugins folder (`SD_PLUGINS_DIR`, `SD_PLUGINS_STATE_DIR`).

- `plugin-snake.webp` (840 × 920): `snake --surface game`, with a saved best
  score of 27 (`--values`), after clicking the board and steering with the
  arrow keys (`--do`).
- `plugin-reminder.webp` (840 × 632): `reminder --surface add` after picking
  "30 min" and typing the text (`--do`).
- `plugin-bell.webp` (104 × 72) and `plugin-pomodoro-item.webp` (184 × 72): the
  `reminder` and `pomodoro` top-bar items at `--height 36`, the timer running.
- `plugin-clock.webp` (144 × 660): `clock-dock --surface main --height 330`.

Converted from PNG to WebP at quality 88 with ImageMagick, without resizing.

## Social preview card (`../media/social-card.jpg`)

The 1200 × 630 image that link previews show (Open Graph and X/Twitter
`summary_large_image`) for `index.html`, `connect.html` and `cli.html`. It was
drawn as an HTML page in headless Chromium on 2026-10-06: the brand, the hero
headline and `readme-overlay.webp` (a real capture, see above) in perspective,
in Noto Sans and JetBrains Mono, then saved as JPEG at quality 88. Keep text
away from the edges: some sites crop the card a little.

## SD Editor

- `sd-editor.webp`: 1240 × 760, the SD Editor card alone, drawn by the
  app's own GTK code (release 1.7.0) on a private Broadway display rather
  than captured from the screen, open on a copy of this repository's files
  with `src/editor/media.rs` showing, in the default Tokyo Night theme.
  WebP at quality 92.

## SD Launcher images

- `sd-launcher-page.webp`: a SUPER DESKTOP home page with live Codex and Claude
  Code sessions.
- `sd-launcher-terminal.webp`: a terminal shell open in the page, with its key row.
- `sd-launcher-new.webp`: starting a new agent on the PC.
- `sd-launcher-menu.webp`: the home screen's long-press menu with the super
  desktop row; the home screen behind the menu is blurred.

Captured on 2026-10-18 with `adb exec-out screencap` on a Xiaomi 2602BPC18G
(1280 × 2772) running SD Launcher in the Tokyo Night theme, resized to 540 px
wide and encoded as WebP at quality 82.
