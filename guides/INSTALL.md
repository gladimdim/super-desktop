[← Back to README](../README.md)

# Installation, Packages, Versions & Power

## Updating and installing from a clone

To update later, run the install command again: it moves the source to the
newest release, rebuilds it, and restarts SUPER DESKTOP if it is running. Work
that is on `master` but not yet released is not installed. **⚙ Settings →
Updates** compares this build's version with the newest on GitHub and lists
what a newer version brings. **Update** rebuilds the source on the newer
version; SUPER DESKTOP restarts on the new build only once the build succeeds,
and a notification says how it went (the log is
`~/.local/state/super-desktop/update.log`). Either way your settings, notes and
running harnesses are kept, and a clone with uncommitted changes or commits of
its own is not updated.

Builds up to `v1.1.21` show an install of a release as "Pinned to vX.Y.Z" in
Settings → Updates, and their **Switch to** button moves it to `master` once.
Running the install command again brings it back to the releases.

**Installing from a clone.** If you work on the code, clone the repository and
run `./install.sh` inside it. The installer then builds and installs that clone
and leaves its git state alone; update it with `git pull && ./rebuild.sh`.
Running the one-line command later moves the clone your install runs from to
the newest release, unless it has local changes or commits of its own.

## Package installations & Linux distro support

`install.sh` auto-detects **Omarchy**, **Arch Linux**, **Fedora 45+**, **Ubuntu 26.04+ / Debian `sid`**, and **openSUSE Tumbleweed**:

- **Omarchy & Arch Linux:** Provides prebuilt, GPG-signed Pacman binary packages (`super-desktop-bin`) for both **`x86_64`** and **`aarch64`** — no Rust toolchain, `cargo`, or C/C++ compilation required. Running `install.sh` sets up the repository and installs `super-desktop-bin` automatically (set `SUPER_DESKTOP_BUILD_FROM_SOURCE=1` to compile from source instead).
- **Fedora 45+, Ubuntu 26.04+ / Debian `sid`, and openSUSE Tumbleweed:** `install.sh` installs build and runtime dependencies via `dnf`, `apt`, or `zypper` (verifying `gtk4 >= 4.18`, `vte-2.91-gtk4 >= 0.84`, and `gtk4-layer-shell` — Ubuntu 24.04 LTS and Debian 13 are rejected early with clear version diagnostics), builds release binaries + `sd-qml-host`, and configures `~/.config/hypr/hyprland.conf` if present or prints instructions for Sway, Niri, or other `wlr-layer-shell` Wayland compositors (GNOME/Mutter and X11 are not supported).

One-line signed repository setup (without the full `install.sh` wrapper):

```bash
curl -fsSL https://superdesktop.dmytrogladkyi.com/install/pacman-repo.sh | bash
```

Or register the signed repository manually step by step:

1. Download and verify the signing key (`EBCB860628250D70F3C4D033304840D354676353`):

```bash
curl -fsSL https://superdesktop.dmytrogladkyi.com/repo/super-desktop.gpg -o /tmp/super-desktop.gpg
gpg --show-keys --with-fingerprint /tmp/super-desktop.gpg
sudo pacman-key --add /tmp/super-desktop.gpg
sudo pacman-key --lsign-key EBCB860628250D70F3C4D033304840D354676353
```

2. Add `/etc/pacman.d/super-desktop.conf` and include it from `/etc/pacman.conf`:

```bash
printf '[super-desktop]\nSigLevel = Required DatabaseRequired\nServer = https://superdesktop.dmytrogladkyi.com/repo/$arch\n' | sudo tee /etc/pacman.d/super-desktop.conf
grep -qxF 'Include = /etc/pacman.d/super-desktop.conf' /etc/pacman.conf || echo 'Include = /etc/pacman.d/super-desktop.conf' | sudo tee -a /etc/pacman.conf
```

3. Optional Omarchy refresh hook (keeps the `Include` line intact if you switch Omarchy channels with `omarchy refresh pacman`):

```bash
mkdir -p ~/.config/omarchy/hooks/pre-refresh-pacman.d
cat <<'EOF' > ~/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop
#!/bin/bash
CONF=/etc/pacman.conf
SNIPPET=/etc/pacman.d/super-desktop.conf
MARKER="Include = $SNIPPET"
[[ -r $SNIPPET ]] || exit 0
grep -qxF "$MARKER" "$CONF" && exit 0
if grep -q '^\[omarchy\]' "$CONF"; then
  sudo sed -i "0,/^\[omarchy\]/s||$MARKER\n\n[omarchy]|" "$CONF"
else
  printf '\n%s\n' "$MARKER" | sudo tee -a "$CONF" >/dev/null
fi
EOF
chmod 755 ~/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop
```

4. Install `super-desktop-bin` and run the user setup:

```bash
sudo pacman -Syu super-desktop-bin
super-desktop-setup
```

An Arch package installation uses `/usr/bin/super-desktop` and keeps the
application files under `/usr/lib/super-desktop` and `/usr/share/super-desktop`.
For an existing `install.sh` source installation, use
`/usr/bin/super-desktop-setup --migrate`. It backs up the old per-user launcher
and desktop entry, routes the launcher to the package, and preserves existing
shortcuts, notes, settings and source checkouts. Custom launcher scripts require
manual migration. Setup requires Omarchy's Lua Hyprland configuration.

Update package installations through **Omarchy's system update** (`omarchy
update`) or `sudo pacman -Syu super-desktop-bin`. Restart the overlay afterwards, or log out and back in:

```bash
/usr/bin/super-desktop kill && /usr/bin/super-desktop toggle
```

Running harnesses remain in tmux while the overlay restarts. Package installation
and removal do not change per-user configuration; removing a package leaves its
shortcut, autostart and theme hook for you to remove if no longer needed.

Binary release packages include dependency license notices and a link to their
corresponding source under `/usr/share/licenses/super-desktop-bin/`. The matching
`super-desktop-X.Y.Z-source.tar.gz` release asset includes the application source,
locked Rust dependencies and instructions for building without downloading crates.

## Installing a specific version

The phone app and the PC talk over a protocol that must match, so if your
phone app is older you may want the PC build made for it. Every release is a
git tag named `vX.Y.Z` with [release notes on GitHub](https://github.com/gladimdim/super-desktop/releases)
that give what changed, the protocol version, and the phone app versions that
work with it. Install one with the same command plus
`SUPER_DESKTOP_VERSION`:

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | SUPER_DESKTOP_VERSION=v1.1.17 bash
```

This works for a first install and on a PC that already has SUPER DESKTOP
(`v1.1.17` and `1.1.17` are the same). The source then stays on that release:
running the plain install command again keeps it, and **⚙ Settings → Updates**
shows "Pinned to v1.1.17" and offers **Switch to** the newest. To move to the
newest release yourself:

```bash
curl -fsSL https://raw.githubusercontent.com/gladimdim/super-desktop/master/install.sh | SUPER_DESKTOP_VERSION=latest bash
```

Settings → Updates only understands a pinned install in releases newer than
`v1.1.17`; on `v1.1.17` itself use the install command above to move. A clone
with uncommitted changes is left as it is. Install the same release on every
PC you connect to each other.

To try changes that are not released yet, `SUPER_DESKTOP_VERSION=master`
follows the `master` branch instead, until you install a release again with
`latest` or a version.

| SUPER DESKTOP | Protocol |
| --- | --- |
| `v2.2.0` | 3 |
| `v2.1.2` | 3 |
| `v2.1.1` | 3 |
| `v2.1.0` | 3 |
| `v2.0.8` | 3 |
| `v2.0.7` | 3 |
| `v2.0.6` | 3 |
| `v2.0.5` | 3 |
| `v2.0.4` | 3 |
| `v2.0.3` | 3 |
| `v2.0.2` | 3 |
| `v2.0.1` | 3 |
| `v2.0.0` | 3 |
| `v1.9.0` | 3 |
| `v1.8.0` | 3 |
| `v1.7.2` | 3 |
| `v1.7.1` | 3 |
| `v1.7.0` | 3 |
| `v1.6.0` | 3 |
| `v1.5.0` | 3 |
| `v1.4.0` | 3 |
| `v1.3.1` | 3 |
| `v1.3.0` | 3 |
| `v1.2.0` | 3 |
| `v1.1.21` | 3 |
| `v1.1.17` | 3 |

Look up your phone app's version in Android's **App info** for SUPER DESKTOP.
Each release's notes give the protocol version and the phone app versions it was
checked with. From `v1.1.17` on, the protocol only grows, so a newer PC build is
meant to keep working with older phone apps; pick an older release when its
notes say your phone app needs it.

## Sleep lock on charger

In **Settings → Sleep lock**, enable **Prevent sleep while plugged in** to keep
the bridge and AI harnesses reachable while SUPER DESKTOP runs on mains or charger
power, including with the laptop lid closed. Recognized desktop chassis without
power-supply entries are treated as mains-powered. The setting persists across app restarts and defaults off.
It releases the lock within a few seconds of unplugging, disabling the setting,
or losing power-source information. Screen locking and display power saving
continue normally. Disable the setting before manually suspending.

This uses systemd-logind's `idle:sleep:handle-lid-switch` block inhibitor; no root
access, logind configuration edits, or helper service is required. The settings
page reports active, battery, unknown-power, or permission/service errors rather
than claiming protection when the lock could not be acquired. Forced sleep and
other components that bypass logind inhibitors are outside its control.

## System launch on a headless PC

In **Settings → System launch**, turn on **Start the daemon at login** so the
daemon starts at every login of this user, including a text console. On a PC
with no display no overlay window appears, but the PC still works from your
phone: the daemon brings back your saved cards (each agent resumes its
conversation), and the phone lists them, opens them, creates new ones and
closes them. Agents find the same programs as in your login shell. This is how
a PC you only reach from your phone stays useful, since Hyprland's own login
autostart needs a Hyprland session and a headless PC starts none. When you
later log in to Hyprland, its overlay takes over and shows the cards that are
already running; after you log out, the headless daemon takes them back.
Notes, card layout and anything else that needs the overlay wait for a desktop
session, and so does opening this PC's desktop from another PC. The setting
installs exactly one private systemd user service,
`~/.config/systemd/user/super-desktop-boot.service`, and turning the setting
off removes that file. The service starts when this user's systemd manager
starts. For a PC that answers straight from power-on with nobody logged in,
turn on lingering once with `loginctl enable-linger $USER`; otherwise it
starts at the first login (a text console, a getty autologin or a display
manager). Hyprland's autostart keeps
working alongside; only one daemon ever runs. The setting is off by default,
and the page shows what systemd reports, plus why a change failed if it did.
