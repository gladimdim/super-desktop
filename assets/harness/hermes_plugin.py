"""SUPER DESKTOP status, prompt, and title plugin for Hermes Agent."""

import json
import os
import subprocess

_HOOK_MAP = {
    "on_session_start": "session_start",
    "on_session_end": "session_end",
    "on_session_reset": "session_reset",
    "pre_llm_call": "pre_llm",
    "post_llm_call": "post_llm",
    "pre_tool_call": "pre_tool",
    "post_tool_call": "post_tool",
    "pre_approval_request": "approval_request",
    "post_approval_response": "approval_response",
    "api_request_error": "api_error",
}


def _extract_user_message(kwargs):
    for key in ("user_message", "prompt", "message"):
        val = kwargs.get(key)
        if isinstance(val, str) and val.strip():
            return val
    msgs = kwargs.get("messages")
    if isinstance(msgs, list):
        for msg in reversed(msgs):
            if isinstance(msg, dict) and msg.get("role") == "user":
                content = msg.get("content")
                if isinstance(content, str) and content.strip():
                    return content
                break
    return ""


def _emit(event_name, kwargs):
    exe = os.environ.get("SD_HARNESS_EXE")
    harness_file = os.environ.get("SD_HARNESS_FILE")
    if not exe or not harness_file:
        return
    ctx = kwargs.get("context")
    session_id = (
        kwargs.get("session_id")
        or (getattr(ctx, "session_id", None) if ctx is not None else None)
        or os.environ.get("HERMES_SESSION_ID")
        or ""
    )
    model = kwargs.get("model") or (getattr(ctx, "model", None) if ctx is not None else None) or ""
    title = kwargs.get("title") or (getattr(ctx, "title", None) if ctx is not None else None) or ""
    user_message = _extract_user_message(kwargs) if event_name == "pre_llm" else ""
    payload = {
        "hook": event_name,
        "session_id": str(session_id or ""),
        "model": str(model or ""),
        "title": str(title or ""),
        "prompt": user_message,
    }
    try:
        subprocess.run(
            [exe, "harness-event", "hermes"],
            input=json.dumps(payload).encode("utf-8"),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
    except Exception:
        pass


def register(ctx):
    for hook_name, mapped in _HOOK_MAP.items():
        def _make_handler(mapped_name):
            def _handler(**kwargs):
                _emit(mapped_name, kwargs)
            return _handler
        try:
            ctx.register_hook(hook_name, _make_handler(mapped))
        except Exception:
            pass
