[← Back to README](../README.md)

# Plugins (QML Surfaces)

Plugins add things to the overlay: docks at the screen edges, items in the
top bar with popups under them, floating panels, cards on the desktop
(games, tools, terminals), full-screen views, sections in Settings and
buttons on terminal cards; they can also move, size, minimize and expand
terminal cards, and close one after asking you on the card. A
plugin is a folder of QML in `~/.config/super-desktop/plugins/<id>/` with a
`plugin.json`, drawn by Qt Quick (`qt6-declarative`) and colored by your
Omarchy theme; it reloads on screen whenever one of its files is saved.

- **Make one with AI**: ✨ **Create Plugin** (top bar, or **Settings →
  Plugins**) asks what it should do and where it goes, then opens an agent
  (Claude Code, Codex…) in a new plugin folder with the plugin skill. The
  plugin appears and changes while the agent works. Agents check their own
  work with `super-desktop plugin check` and `plugin snapshot`, which also
  clicks and types through a plugin like a user.
- **Get plugins**: **Settings → Plugins → Discover** lists the public
  [plugin list](https://github.com/gladimdim/super-desktop-plugins) with
  pictures, a search and categories. **Install** clones a plugin from its
  repository; it runs only after you approve it, and **Ask AI to review**
  reads it for you first. `super-desktop plugin search` and `plugin install
  <id>` do the same in a terminal.
- **Settings → Plugins** lists your plugins in narrow rows (a small
  picture, name, version, a one-line description and an on/off switch). A
  click on a row opens the plugin's page: its own settings, Open, Edit &
  Enhance, its log and Delete.
- **Edit & Enhance** on any plugin opens an agent to improve it. Someone
  else's plugin first becomes your own copy (it keeps its saved data and
  stops updating from its author).
- **Share yours**: `super-desktop plugin preview <id>` draws its picture and
  `plugin publish <id>` checks it is ready for the list; the list's
  [CONTRIBUTING.md](https://github.com/gladimdim/super-desktop-plugins/blob/main/CONTRIBUTING.md)
  explains the pull request.

Plugins run as you: their QML can start programs. Plugins you make run at
once; installed ones wait for your approval. See the
[plugin commands](../CLI.md#plugins).
