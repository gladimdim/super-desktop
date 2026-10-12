"""Card Kit: titles every card "🧪 <agent>" with a chip; its card button types
a line into the terminal and reads the screen back. Events go to
$SD_PLUGIN_DATA/events.log."""
import os
import time

from sd_plugin import Plugin

plugin = Plugin()
LOG = os.path.join(os.environ["SD_PLUGIN_DATA"], "events.log")


def record(line):
    with open(LOG, "a") as f:
        f.write(line + "\n")


@plugin.on("title.inputs")
def inputs(cards):
    for card in cards:
        plugin.call("title.set", card=card["id"], text=f"🧪 {card['agent']} · {card['status']}",
                    chipsBefore=[{"text": "kit", "tone": "accent", "tooltip": "set by Card Kit"}])
        record(f"titled {card['id']}")


@plugin.command("cardkit.type")
def type_line(context):
    card = context["card"]["id"]
    described = plugin.call("workspace.cards")
    record(f"cards {len(described['cards'])} screen {described['screen']['w']}x{described['screen']['h']}")
    plugin.call("terminal.send", card=card, text="echo plugin-was-$((40+2))", enter=True)
    for _ in range(50):
        text = plugin.call("terminal.text", card=card, lines=20)["text"]
        if "plugin-was-42" in text:
            record("read back plugin-was-42")
            return
        time.sleep(0.1)
    record("never read back")


@plugin.command("cardkit.launch")
def launch(context):
    args = context["args"]
    reply = plugin.call("harness.launch", agent=args["agent"], folder=args["folder"], prompt=args["prompt"])
    record(f"launched {reply['card']}")


plugin.run()
