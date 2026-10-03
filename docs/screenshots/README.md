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
- `phone-confirm.webp`, `phone-request.webp`, `phone-code.webp`,
  `phone-connected.webp`: `adb exec-out screencap` from a 1280 × 2772 phone,
  cropped below the status bar to 1280 × 1800 and scaled to 640 × 900.

The phone reached the disposable bridge through `adb reverse`, then over
Tailscale. IP addresses, and the names of other saved computers, were blurred
after OCR (tesseract) located them. The verification code is the one that
pairing really used; the invitation in the QR code has expired and its bridge
no longer exists. All images are WebP at quality 90–92.
