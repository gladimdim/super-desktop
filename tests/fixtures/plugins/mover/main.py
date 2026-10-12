"""Mover: `plugin run mover mover.place '{"card": ID, "rect": {...}}'` and
`mover.iconify '{"card": ID, "at": {...}}'`, for tests that cannot drag."""
from sd_plugin import Plugin

plugin = Plugin()


@plugin.command("mover.place")
def place(context):
    args = context["args"]
    plugin.call("card.setRect", card=args["card"], rect=args["rect"])


@plugin.command("mover.iconify")
def iconify(context):
    args = context["args"]
    plugin.call("card.iconify", card=args["card"], at=args["at"])


plugin.run()
