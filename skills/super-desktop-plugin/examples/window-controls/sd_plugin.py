"""Minimal SUPER DESKTOP plugin runtime for Python 3.9+ (plugin API 1).

Copy this file next to your plugin's main.py. Standard library only.

JSON-RPC 2.0 over stdin/stdout, one JSON object per line. stdout carries the
protocol only, so this module moves print() to stderr, which the host keeps in
the plugin's log.

    from sd_plugin import Plugin

    plugin = Plugin()

    @plugin.command("my-plugin.hello")
    def hello(context):
        plugin.call("ui.notify", title="Hello", body="from my plugin")

    plugin.run()

Handlers run on worker threads, so they may call plugin.call() (which blocks
until the host answers). Keep 'activate' and 'deactivate' fast: the host waits
10 s and 1 s for them.
"""

import itertools
import json
import sys
import threading
import traceback

API_VERSION = 1
MAX_MESSAGE = 1024 * 1024


class RpcError(Exception):
    """An error answer from the host. data usually has reason, hint and docs."""

    def __init__(self, code, message, data=None):
        super().__init__(f"{code} {message}: {data}")
        self.code = code
        self.message = message
        self.data = data or {}


class Plugin:
    def __init__(self):
        self._out = sys.stdout
        sys.stdout = sys.stderr  # stray print() must never corrupt the protocol
        self._write_lock = threading.Lock()
        self._ids = itertools.count(1)
        self._pending = {}
        self._pending_lock = threading.Lock()
        self._handlers = {}
        self._commands = {}
        self._view_handlers = {}
        self._on_activate = None
        self._on_deactivate = None
        self.stopping = threading.Event()
        self.info = {}

        self._handlers["activate"] = self._activate
        self._handlers["deactivate"] = self._deactivate
        self._handlers["command"] = self._command
        self._handlers["view.event"] = self._view_event
        self._handlers["view.opened"] = self._view_opened

    # ---- registration -------------------------------------------------
    def on(self, method):
        """Handle a host->plugin method or notification, e.g. 'title.inputs'."""

        def register(fn):
            self._handlers[method] = lambda params: fn(**params)
            return fn

        return register

    def command(self, command_id):
        def register(fn):
            self._commands[command_id] = fn
            return fn

        return register

    def view(self, view_id):
        """fn(handle, node, event, value) for events from one declared view.

        Also called with node "root", event "open" (value: the anchor id) when the
        host opened the view from a toolbar item: fill it with ui.patch.
        """

        def register(fn):
            self._view_handlers[view_id] = fn
            return fn

        return register

    def on_activate(self, fn):
        self._on_activate = fn
        return fn

    def on_deactivate(self, fn):
        self._on_deactivate = fn
        return fn

    # ---- calls to the host --------------------------------------------
    def call(self, method, timeout=150.0, **params):
        msg_id = next(self._ids)
        waiter = {"event": threading.Event()}
        with self._pending_lock:
            self._pending[msg_id] = waiter
        self._send({"jsonrpc": "2.0", "id": msg_id, "method": method, "params": params})
        if not waiter["event"].wait(timeout):
            with self._pending_lock:
                self._pending.pop(msg_id, None)
            raise RpcError(-32005, "timeout", {"reason": f"{method} got no answer in {timeout} s"})
        if "error" in waiter:
            err = waiter["error"]
            raise RpcError(err.get("code"), err.get("message"), err.get("data"))
        return waiter.get("result")

    def notify(self, method, **params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def log(self, message, level="info"):
        self.notify("log", level=level, message=str(message)[:4096])

    # ---- loop ---------------------------------------------------------
    def run(self):
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            if len(line) > MAX_MESSAGE:
                self.log("dropped an oversized message from the host", "warn")
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                self.log("host sent a line that is not JSON", "error")
                continue
            if "method" in msg:
                threading.Thread(target=self._dispatch, args=(msg,), daemon=True).start()
            else:
                self._resolve(msg)
        self.stopping.set()

    # ---- internals ----------------------------------------------------
    def _send(self, msg):
        data = json.dumps(msg, separators=(",", ":"), ensure_ascii=False)
        if len(data.encode("utf-8")) > MAX_MESSAGE:
            raise ValueError("message larger than 1 MiB; split it (ui.patch) or shorten it")
        with self._write_lock:
            self._out.write(data + "\n")
            self._out.flush()

    def _resolve(self, msg):
        with self._pending_lock:
            waiter = self._pending.pop(msg.get("id"), None)
        if waiter is None:
            return
        if "error" in msg:
            waiter["error"] = msg["error"]
        else:
            waiter["result"] = msg.get("result")
        waiter["event"].set()

    def _dispatch(self, msg):
        method, msg_id = msg["method"], msg.get("id")
        params = msg.get("params") or {}
        handler = self._handlers.get(method)
        try:
            if handler is None:
                if msg_id is not None:
                    self._send({"jsonrpc": "2.0", "id": msg_id,
                                "error": {"code": -32601, "message": f"plugin does not handle {method}"}})
                return
            result = handler(params)
            if msg_id is not None:
                self._send({"jsonrpc": "2.0", "id": msg_id, "result": result if result is not None else {}})
        except Exception as exc:  # report, never crash the loop
            traceback.print_exc()
            if msg_id is not None:
                self._send({"jsonrpc": "2.0", "id": msg_id,
                            "error": {"code": -32000, "message": str(exc)[:500]}})

    def _activate(self, params):
        if params.get("apiVersion") != API_VERSION:
            raise RuntimeError(f"plugin API {params.get('apiVersion')} is not supported by this runtime")
        self.info = params
        if self._on_activate:
            self._on_activate(params)
        return {}

    def _deactivate(self, _params):
        self.stopping.set()
        if self._on_deactivate:
            self._on_deactivate()
        return {}

    def _command(self, params):
        fn = self._commands.get(params.get("command"))
        if fn is None:
            self.log(f"no handler for command {params.get('command')}", "warn")
            return
        fn(params.get("context") or {})

    def _view_opened(self, params):
        """A toolbar item that names a view was clicked: the handler gets event "open"."""
        fn = self._view_handlers.get(params.get("view"))
        if fn is not None:
            fn(params.get("handle"), "root", "open", params.get("anchor"))

    def _view_event(self, params):
        fn = self._view_handlers.get(params.get("view"))
        if fn is not None:
            fn(params.get("handle"), params.get("node"), params.get("event"), params.get("value"))
