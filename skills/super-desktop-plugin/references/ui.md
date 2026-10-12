# Views: declarative UI

Plugins never get GTK widgets, HTML or CSS. They describe a view as a tree of
nodes and the host builds themed widgets from it, so every plugin follows the
Omarchy theme and none can imitate SUPER DESKTOP's own dialogs.

Normative schema: `schemas/ui.schema.json`.

## Declaring and opening

```json
"views": [{ "id": "my-plugin.main", "title": "My Plugin", "kind": "panel", "width": 640, "height": 480 }]
```

- `panel`: a movable, resizable card with the theme's header and a ✕. It
  remembers its size and place. Hidden with the overlay.
- `popover`: anchored to the toolbar item or card button that opened it
  (pass `anchor` to `ui.open`). Closed when the overlay hides.

`ui.open {view, model}` → `{handle}`. Keep the handle; patch with it; forget it
on `view.closed`. Opening a view that is already open replaces its content with
the new model, raises it and returns the same handle (a "Refresh" can simply
open it again).

A toolbar item with `"view"` instead of `"command"` opens the view itself: the
host shows a placeholder (a `column` with id `root` holding a `spinner`) and
sends `view.opened {handle, view, anchor}`. Fill it with
`ui.patch {handle, ops: [{"op": "replace", "id": "root", "node": {...}}]}`;
your node's id can be `root` again.

## Nodes

Every node has `type` and `id` (unique in the view, `[A-Za-z0-9_.:-]{1,64}`)
and may have `visible`.

| Type | Props | Events |
| --- | --- | --- |
| `group` | `title`, `subtitle`, `tone` (`neutral`, `accent`, `success`, `warning`, `error`), `gap`, `children` | |
| `column`, `row` | `gap` (0–32), `children` | |
| `scroll` | `maxHeight`, `children` | |
| `list` | `gap`, `children` (rows separated by the theme) | |
| `label` | `text`, `style` (`title`, `body`, `muted`, `error`, `success`, `mono`), `wrap` | |
| `markdown` | `text` (CommonMark; links open in the browser after a confirm) | |
| `code` | `text` (monospace, selectable) | |
| `badge` | `text` (≤ 24), `tone` (`neutral`, `accent`, `success`, `warning`, `error`) | |
| `icon` | `name` (emoji or plugin-relative image), `size` | |
| `button` | `label`, `icon`, `tone` (`default`, `primary`, `danger`), `enabled` | `click` |
| `toggle`, `checkbox` | `label`, `value`, `enabled` | `change` (bool) |
| `entry` | `value`, `placeholder`, `enabled` | `change` (after 300 ms idle), `submit` (Enter) |
| `textArea` | `value`, `rows`, `enabled` | `change` |
| `select` | `value`, `options: [{value, label}]` | `change` |
| `progress` | `value` (0–1, or `null` for indeterminate) | |
| `spinner` | | |
| `spacer` | (takes the free space in a row: what follows goes to the right) | |
| `separator` | | |

Limits: 2000 nodes per view, 500 children per node, 64 KiB of text per node.

## Patching

`ui.patch {handle, ops}` with up to 500 operations, applied in order:

```json
[
  { "op": "set", "id": "status:2", "props": { "text": "pushed 1a2b3c4", "style": "success" } },
  { "op": "append", "id": "rows", "node": { "type": "label", "id": "note", "text": "Done" } },
  { "op": "replace", "id": "rows", "node": { "type": "list", "id": "rows", "children": [] } },
  { "op": "remove", "id": "note" }
]
```

Patch what changed instead of reopening the view: it keeps scroll position,
focus and text the user is typing. A `set` changes only the props it names
(`{"visible": true}` on a button needs no label); a `replace` or `append`
node is complete and needs its required props. An op naming a node the view
does not have fails the whole patch with an error saying which op. A value the user is editing is not
overwritten by `set` on the same prop while the field has focus.

## Look

Views are drawn with the active Omarchy theme and follow theme switches; a
plugin never chooses colours. Use the structure to get a clean, modern panel:

- **Group what belongs together.** A `group` is a bordered panel (title and
  subtitle optional). Give the one that needs attention a `tone`: `accent`
  for the main area, `warning` / `error` for problems, `success` for done.
- **One row per item, actions at the end:** `row` → content, `spacer`,
  buttons. Use `badge`s for short facts (a branch, a count) instead of long
  sentences.
- **One primary button per view** (`tone: "primary"`); `danger` for actions
  that destroy or publish; plain buttons for the rest.
- Text styles: `title` for headings, `muted` for secondary text, `mono` for
  paths and ids, `code` blocks for command output and file lists.

## Patterns

- **Long jobs**: show a `spinner`, disable the action buttons, update one
  row's status label per step, then re-enable. Every failure appears on its
  row; one failure does not stop the others.
- **Review before acting**: for anything that writes, sends or deletes, show
  what will happen (editable where sensible) and wait for a `primary` or
  `danger` button. Git Flush is the model.
- **Empty state**: say what to do ("Add folders in Settings → Plugins → …").
- Text is plain; use `markdown` only for content that is Markdown.
