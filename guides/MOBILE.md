[← Back to README](../README.md)

# Android App, Free APK & Phone Bridge

## Phone bridge overview

For the [SUPER DESKTOP Android app on Google Play](https://play.google.com/store/apps/details?id=com.gladimdim.superdesktop) ([how to connect it](https://superdesktop.dmytrogladkyi.com/connect.html)). Encrypted HTTPS/WSS over LAN or Tailscale (port 8759, mDNS `_omarchy-harness._tcp`). It starts automatically with the SUPER DESKTOP daemon; stop or restart it from the ⚙ gear → *Connections*. A background supervisor checks every five seconds and recovers the bridge after three consecutive failed checks, even with the overlay hidden or screen locked. Stop pauses recovery until Start (or the next daemon launch). Failed starts are retried, and `bridge.previous.log` preserves the previous run beside `bridge.log`. To stay reachable while unattended on external power, enable *Settings → Sleep lock*; recovery cannot run while the PC is suspended. Desktop QR verification, explicit approval, and per-phone revocation keep access under your control. A TV, which cannot scan the QR code, pairs with a code instead: ⚙ → *Connections* → *Add a device* → *Pair a TV*, then type the code the TV shows. Tailscale is optional. Protocol v3 requires re-pairing older phones. See [Security](../SECURITY.md).

## Free Android APK

[Download SUPER DESKTOP for Android](https://github.com/gladimdim/super-desktop/releases/download/v2.0.8/SUPER-DESKTOP-2.2.0.apk): every feature is free forever,
with no 10-day trial, Pro purchase or subscription. Google Play is not required.
Android 10 or newer; phones, foldables, tablets and TVs are supported.
Open the downloaded APK and allow installation from your browser when Android
asks. Install future updates from GitHub Releases.

The APK installs alongside the Google Play app, with separate settings and
pairings. Existing SUPER DESKTOP SELF installations update in place. The Google
Play edition keeps its usage-day trial and one-time Pro purchase.
[Connect your phone](https://superdesktop.dmytrogladkyi.com/connect.html).

The free APK uses protocol 3 with no new PC minimum. Individual features retain
their requirements: TV code pairing needs PC 1.8.0+, web previews need 1.9.0+.
**2.2.0 (20)** adds Antigravity CLI shortcuts above the keyboard. Tap
**/resume** and **Send** for Antigravity’s conversation picker. New direct
Antigravity sessions on PC **2.0.8** use inline scrollback when the CLI supports
per-session rendering settings.

**2.1.0 (19)** adds voice prompts: in an AI harness terminal, tap the microphone,
speak, and send the clip with your prompt. The agent transcribes it on your PC
with a speech-to-text tool installed there (for example whisper.cpp). The app
asks for the microphone on first use. Protocol 3 and the existing PC
requirements apply. Checked on an Android 14 emulator against PC 2.0.7.

**2.0.3 (18)** reconnects Sessions when the phone changes networks. Retry tries
each saved connectable address and shows **Cannot connect** if they all fail.
On a physical keyboard, Ctrl+Enter sends and Enter adds a new line.
Protocol 3 applies with the existing PC requirements. No released PC was
checked end to end for this patch.

**2.0.2 (17)** separates Send from the message field and adds physical-keyboard
Enter to send (Shift+Enter for a new line). Holding Enter does not repeat sends.
Protocol 3 and PC requirements are unchanged. The fixes were confirmed on
Xiaomi Pad 7 Pro; no released PC version range was separately tested.

**2.0.1 (16)** makes harness cards more compact and refreshes Files & links
with clearer file rows, contextual sharing and PDF paging controls. Protocol 3
is unchanged; no new PC minimum. No released PC was checked end to end for this
patch. The GitHub APK remains free forever.

**2.0.0 (15)** brings Studio: clearer session cards, usage values, Settings and
a refreshed composer, with all Omarchy themes retained. **＋ → Camera** takes
a photo, adds its preview to the draft, and sends it only when you press Send.
Camera capture needs a compatible camera app and the existing camera permission.
No protocol change or new PC minimum. Compatibility checked on Android 14
emulators: PC **2.0.5** with released Play **1.5.1 (12)** and new Play
**2.0.0 (15)** (pairing, sessions, terminal output and text prompts); the new
build also sent a camera photo to the PC. Oldest/newest PC tested: 2.0.5.
The separate Play build keeps its usage-day trial and one-time Pro unlock.

## Sending images, files, and voice prompts from Android

Android's per-terminal bell can report explicit Codex, Claude Code, Pi and OpenCode response completion,
including while the phone UI is hidden using an opt-in foreground monitor. Pi
requires a new launch with the updated desktop extension and updated Android app.
Other harnesses do not yet have verified completion adapters.

Android can send images and files with a prompt to any harness, whether it is idle or
still working (a working agent queues it for its next turn, exactly as typed input is
queued): **＋**
picks an image or a file or pastes an image from the clipboard (pasting into
the text field and dropping a file on a terminal work too). **Share → SUPER
DESKTOP** in any other Android app asks which harness gets the files or text,
and adds them to its draft. Up to 4 attachments and 16 MB go with one prompt.
The PC keeps them privately in `~/.local/state/super-desktop/uploads/`,
outside every workspace. Codex, Claude
Code, Grok and OpenCode's full interface receive images as their own image
attachments; other harnesses, and all other files, get the file's path in the
prompt, and a shell gets the quoted path after the command.

## Terminal history on mobile

New default launches use native scrollback: OpenCode `--mini`, Codex
`--no-alt-screen`, Pi `--tui-mode regular`, and Hermes `--cli`. Explicit custom
mode flags are retained. Existing sessions are not restarted. Crush stays in
its fullscreen UI; Android’s keyboard panel provides Tab to focus its history
and Page Up/Page Down to navigate older output. These keys move the remote
viewport rather than creating a local transcript.

## Grok on mobile

Default Grok launches use `--minimal` so finalized output enters native terminal
scrollback and is visible to the Android snapshot viewer. Fullscreen Grok keeps
its history inside the alternate-screen UI, exposing only its current viewport.
For an existing conversation, use `/minimal` to switch in place without restarting.
With an older launcher, set `[ui] screen_mode = "minimal"` in Grok's config.
Requires a Grok version with minimal mode. The bridge still sends a bounded
history tail; this does not turn it into an unlimited transcript viewer.
