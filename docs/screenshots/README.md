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
