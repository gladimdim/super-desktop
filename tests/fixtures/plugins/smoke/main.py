"""Smoke-test plugin: writes what happens to $SD_PLUGIN_DATA/events.log."""
import json
import os
import subprocess
import sys

from sd_plugin import Plugin

plugin = Plugin()
LOG = os.path.join(os.environ["SD_PLUGIN_DATA"], "events.log")


def record(line):
    with open(LOG, "a") as f:
        f.write(line + "\n")


@plugin.on_activate
def activated(info):
    record("activated " + info["settings"]["greeting"])


@plugin.on_deactivate
def deactivated():
    record("deactivated")


@plugin.command("smoke.open")
def open_panel(context):
    described = plugin.call("host.describe")
    plugin.call("contrib.update", id="smoke.button", badge="1")
    handle = plugin.call("ui.open", view="smoke.panel", model={
        "type": "column", "id": "root", "children": [
            {"type": "label", "id": "status", "text": "ready"},
            {"type": "button", "id": "go", "label": "Go", "tone": "primary"},
        ]})["handle"]
    plugin.call("ui.patch", handle=handle, ops=[{"op": "set", "id": "status", "props": {"text": "patched"}}])
    try:
        plugin.call("ui.patch", handle=handle, ops=[{"op": "set", "id": "missing", "props": {}}])
    except Exception as error:  # the host answers with an error naming the op
        record("patch-error " + json.dumps(getattr(error, "data", {})))
    try:
        plugin.call("terminal.send", card="x", text="y")
    except Exception as error:
        record("denied " + str(getattr(error, "code", "")))
    record(f"opened {handle} api={described['apiVersion']} source={context.get('source')}")


@plugin.command("smoke.crash")
def crash(context):
    record("crashing")
    os._exit(3)


@plugin.command("smoke.spawn")
def spawn(context):
    child = subprocess.Popen(["sh", "-c", "trap '' TERM; sleep 300"])
    record(f"spawned {child.pid}")


plugin.run()
