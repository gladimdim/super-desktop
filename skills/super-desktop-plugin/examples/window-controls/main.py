"""Window Controls: half-screen and edge-icon buttons for every local card.

The buttons act through card.setRect and card.iconify, so the result is saved
like a user drag and stays after the plugin is turned off.
"""

from sd_plugin import Plugin

plugin = Plugin()
GAP = 8
ICON = 96


def target(context):
    workspace = plugin.call("workspace.cards")
    card = context.get("card") or {}
    return workspace["screen"], card.get("id")


def half(context, side):
    screen, card = target(context)
    if not card:
        return
    width = (screen["w"] - 3 * GAP) / 2
    x = GAP if side == "left" else GAP * 2 + width
    plugin.call("card.setRect", card=card,
                rect={"x": x, "y": screen["top"] + GAP, "w": width, "h": screen["h"] - screen["top"] - 2 * GAP})


@plugin.command("window-controls.left")
def left(context):
    half(context, "left")


@plugin.command("window-controls.right")
def right(context):
    half(context, "right")


@plugin.command("window-controls.edge")
def to_edge(context):
    screen, card = target(context)
    if not card:
        return
    rect = context["card"]["rect"]
    centre = rect["x"] + rect["w"] / 2
    x = GAP if centre < screen["w"] / 2 else screen["w"] - ICON - GAP
    y = min(max(rect["y"], screen["top"] + GAP), screen["h"] - ICON - GAP)
    plugin.call("card.iconify", card=card, at={"x": x, "y": y})


plugin.run()
