"""Git Flush: commit and push every dirty repository on its current branch.

Flow: the toolbar badge counts dirty repositories. The popup lists them.
"Flush all" fetches, fast-forwards each current branch from its upstream,
and asks the AI provider for a commit message. Nothing is committed until the
user presses "Commit & push" (unless skipReview is on). Never merges into
another branch, never rebases, never force-pushes.
"""

import hashlib
import json
import os
import subprocess
import threading

from sd_plugin import Plugin, RpcError

plugin = Plugin()
VIEW = "git-flush.repos"
BUTTON = "git-flush.button"
DIFF_LIMIT = 48 * 1024
SECRET_HINTS = (".env", ".pem", ".key", ".p12", "id_rsa", "id_ed25519", "credentials", "secret")

lock = threading.Lock()
state = {"handle": None, "repos": [], "selected": set(), "proposals": {}, "busy": False}


# ---- git -------------------------------------------------------------------
def git(repo, *args, stdin=None, timeout=60):
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_OPTIONAL_LOCKS="0", LC_ALL="C")
    return subprocess.run(["git", "-C", repo, *args], input=stdin, capture_output=True,
                          text=True, timeout=timeout, env=env)


def first_line(proc):
    """The most useful line of git's output: the rejection or error, not "To <remote>"."""
    lines = [line.strip() for line in (proc.stderr or proc.stdout or "").splitlines() if line.strip()]
    for prefix in ("! [rejected]", "! [remote rejected]", "error:", "fatal:"):
        for line in lines:
            if line.startswith(prefix):
                return line[:160]
    return lines[0][:160] if lines else f"git exited with {proc.returncode}"


def settings():
    return plugin.call("settings.get") or {}


def scan(repo, include_untracked):
    info = {"path": repo, "name": os.path.basename(repo.rstrip("/")) or repo, "branch": None,
            "upstream": None, "ahead": 0, "behind": 0, "changed": 0, "untracked": 0, "blocked": None}
    proc = git(repo, "status", "--porcelain=v2", "--branch")
    if proc.returncode != 0:
        info["blocked"] = first_line(proc)
        return info
    conflicts = 0
    for line in proc.stdout.splitlines():
        if line.startswith("# branch.head "):
            info["branch"] = line[len("# branch.head "):]
        elif line.startswith("# branch.upstream "):
            info["upstream"] = line[len("# branch.upstream "):]
        elif line.startswith("# branch.ab "):
            ahead, behind = line[len("# branch.ab "):].split()
            info["ahead"], info["behind"] = int(ahead), -int(behind)
        elif line.startswith(("1 ", "2 ")):
            info["changed"] += 1
        elif line.startswith("u "):
            conflicts += 1
        elif line.startswith("? "):
            info["untracked"] += 1
    if info["branch"] == "(detached)":
        info["blocked"] = "detached HEAD"
    elif conflicts:
        info["blocked"] = f"{conflicts} conflicted file(s)"
    for marker, what in (("MERGE_HEAD", "a merge"), ("rebase-merge", "a rebase"), ("rebase-apply", "a rebase"),
                         ("CHERRY_PICK_HEAD", "a cherry-pick"), ("REVERT_HEAD", "a revert")):
        path = git(repo, "rev-parse", "--git-path", marker).stdout.strip()
        if path and os.path.exists(os.path.join(repo, path) if not os.path.isabs(path) else path):
            info["blocked"] = f"in the middle of {what}"
    info["dirty"] = info["changed"] + (info["untracked"] if include_untracked else 0) + conflicts > 0
    return info


def change_digest(repo):
    """Fingerprint of the uncommitted work, to notice edits made after the message was written."""
    parts = (git(repo, "diff", "HEAD", "--binary").stdout, git(repo, "diff", "--cached", "--name-status").stdout,
             git(repo, "ls-files", "--others", "--exclude-standard", "-s").stdout)
    digest = hashlib.sha256()
    for part in parts:
        digest.update(part.encode() + b"\0")
    for name in git(repo, "ls-files", "--others", "--exclude-standard").stdout.splitlines():
        digest.update(git(repo, "hash-object", "--", name).stdout.encode())
    return digest.hexdigest()


def scan_all():
    values = settings()
    include = values.get("includeUntracked", True)
    repos = [os.path.expanduser(p) for p in values.get("repositories") or []]
    return [r for r in (scan(p, include) for p in repos) if r["dirty"] or r["blocked"]]


def diff_for_llm(repo, include_untracked):
    names = [n for n in git(repo, "diff", "HEAD", "--name-only").stdout.splitlines() if n]
    hidden = [n for n in names if any(h in n.lower() for h in SECRET_HINTS)]
    shown = [n for n in names if n not in hidden]
    text = git(repo, "diff", "HEAD", "--stat").stdout
    if shown:
        text += "\n" + git(repo, "diff", "HEAD", "--", *shown).stdout
    if include_untracked:
        new = git(repo, "ls-files", "--others", "--exclude-standard").stdout.splitlines()
        text += "\nNew files:\n" + "\n".join(new[:200])
        hidden += [n for n in new if any(h in n.lower() for h in SECRET_HINTS)]
    if hidden:
        text += "\nChanged files whose content is not shown: " + ", ".join(hidden[:50])
    return text[:DIFF_LIMIT]


def write_message(repo, include_untracked):
    reply = plugin.call(
        "llm.complete", json=True, maxTokens=400, tier="fast",
        system=("You write git commit messages. Answer with JSON {\"subject\": str, \"body\": str}. "
                "Subject: imperative mood, at most 72 characters, no trailing period. Body: optional, "
                "wrapped at 72 columns, says what changed and why when the diff shows it. Describe only "
                "changes present in the diff."),
        prompt=f"Repository: {os.path.basename(repo)}\n\n{diff_for_llm(repo, include_untracked)}")
    try:
        parsed = json.loads(reply["text"])
        subject, body = parsed.get("subject", "").strip(), parsed.get("body", "").strip()
    except (ValueError, AttributeError):
        subject, body = reply["text"].strip().splitlines()[0][:72], ""
    return subject + ("\n\n" + body if body else "")


def bring_up_to_date(repo):
    """Fetch and fast-forward the current branch. Returns (ok, note)."""
    upstream = git(repo, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}")
    if upstream.returncode != 0:
        return True, "will create origin/" + git(repo, "branch", "--show-current").stdout.strip()
    fetch = git(repo, "fetch", "--quiet", timeout=120)
    if fetch.returncode != 0:
        return False, "fetch failed: " + first_line(fetch)
    counts = git(repo, "rev-list", "--left-right", "--count", "HEAD...@{upstream}").stdout.split()
    ahead, behind = (int(counts[0]), int(counts[1])) if len(counts) == 2 else (0, 0)
    if ahead and behind:
        return False, f"diverged from {upstream.stdout.strip()} (ahead {ahead}, behind {behind})"
    if behind:
        merge = git(repo, "merge", "--ff-only", "@{upstream}")
        if merge.returncode != 0:
            return False, "cannot fast-forward: " + first_line(merge)
    return True, "will push to " + upstream.stdout.strip()


def commit_and_push(repo, message, include_untracked):
    add = git(repo, "add", "-A" if include_untracked else "-u")
    if add.returncode != 0:
        return False, "add failed: " + first_line(add)
    if git(repo, "diff", "--cached", "--quiet").returncode == 0:
        return False, "nothing to commit"
    commit = git(repo, "commit", "--file=-", stdin=message)
    if commit.returncode != 0:
        return False, "commit failed: " + first_line(commit)
    branch = git(repo, "branch", "--show-current").stdout.strip()
    remote = git(repo, "config", f"branch.{branch}.remote").stdout.strip()
    merge_ref = git(repo, "config", f"branch.{branch}.merge").stdout.strip()
    if remote and merge_ref:
        push = git(repo, "push", remote, f"HEAD:{merge_ref}", timeout=180)
    else:
        push = git(repo, "push", "-u", "origin", "HEAD", timeout=180)
    if push.returncode != 0:
        return False, "committed, push failed: " + first_line(push)
    return True, "pushed " + git(repo, "rev-parse", "--short", "HEAD").stdout.strip()


# ---- view ------------------------------------------------------------------
def row(i, repo):
    where = repo["branch"] or "?"
    if repo["upstream"]:
        where += f" → {repo['upstream']}"
    counts = f"{repo['changed']} changed, {repo['untracked']} new"
    status = repo["blocked"] or ""
    return {"type": "column", "id": f"repo:{i}", "gap": 4, "children": [
        {"type": "row", "id": f"head:{i}", "gap": 8, "children": [
            {"type": "checkbox", "id": f"sel:{i}", "value": not repo["blocked"], "enabled": not repo["blocked"],
             "label": repo["name"]},
            {"type": "label", "id": f"where:{i}", "text": where, "style": "muted"},
            {"type": "badge", "id": f"count:{i}", "text": counts[:24]},
        ]},
        {"type": "label", "id": f"status:{i}", "text": status, "style": "error" if repo["blocked"] else "muted",
         "visible": bool(status)},
        {"type": "textArea", "id": f"msg:{i}", "value": "", "rows": 4, "visible": False},
    ]}


def model(repos):
    rows = [row(i, r) for i, r in enumerate(repos)] or [
        {"type": "label", "id": "empty", "text": "No repositories with changes. Add folders in Settings → Plugins → Git Flush.",
         "style": "muted", "wrap": True}]
    return {"type": "column", "id": "root", "gap": 12, "children": [
        {"type": "scroll", "id": "rows-scroll", "maxHeight": 440, "children": [
            {"type": "list", "id": "rows", "gap": 10, "children": rows}]},
        {"type": "row", "id": "actions", "gap": 8, "children": [
            {"type": "button", "id": "refresh", "label": "Refresh"},
            {"type": "button", "id": "flush", "label": "Flush all", "tone": "primary", "enabled": bool(repos)},
            {"type": "button", "id": "commit", "label": "Commit & push", "tone": "danger", "visible": False},
            {"type": "spinner", "id": "busy", "visible": False},
        ]},
    ]}


def patch(*ops):
    if state["handle"]:
        try:
            plugin.call("ui.patch", handle=state["handle"], ops=list(ops))
        except RpcError as err:
            plugin.log(f"ui.patch failed: {err}", "warn")


def set_props(node, **props):
    return {"op": "set", "id": node, "props": props}


def set_busy(busy):
    state["busy"] = busy
    patch(set_props("busy", visible=busy), set_props("flush", enabled=not busy),
          set_props("refresh", enabled=not busy), set_props("commit", enabled=not busy))


def refresh_badge(repos):
    dirty = sum(1 for r in repos if not r["blocked"])
    plugin.call("contrib.update", id=BUTTON, badge=str(dirty) if dirty else None)


# ---- commands and events ---------------------------------------------------
@plugin.command("git-flush.open")
def open_view(_context):
    repos = scan_all()
    with lock:
        state.update(repos=repos, proposals={}, selected={i for i, r in enumerate(repos) if not r["blocked"]})
    refresh_badge(repos)
    state["handle"] = plugin.call("ui.open", view=VIEW, model=model(repos), anchor=BUTTON)["handle"]


@plugin.view(VIEW)
def on_view(handle, node, event, value):
    if node.startswith("sel:") and event == "change":
        i = int(node[4:])
        (state["selected"].add if value else state["selected"].discard)(i)
    elif node.startswith("msg:") and event == "change":
        i = int(node[4:])
        if i in state["proposals"]:
            state["proposals"][i]["message"] = value
    elif node == "refresh" and event == "click" and not state["busy"]:
        open_view({})
    elif node == "flush" and event == "click" and not state["busy"]:
        threading.Thread(target=analyze, daemon=True).start()
    elif node == "commit" and event == "click" and not state["busy"]:
        threading.Thread(target=commit_all, daemon=True).start()


def analyze():
    set_busy(True)
    include = settings().get("includeUntracked", True)
    state["proposals"] = {}
    for i, repo in enumerate(state["repos"]):
        if i not in state["selected"] or repo["blocked"]:
            continue
        patch(set_props(f"status:{i}", text="Checking upstream…", style="muted", visible=True))
        ok, note = bring_up_to_date(repo["path"])
        if not ok:
            patch(set_props(f"status:{i}", text=note, style="error"))
            continue
        patch(set_props(f"status:{i}", text="Writing a commit message…"))
        try:
            message = write_message(repo["path"], include)
        except RpcError as err:
            hint = err.data.get("hint") or err.message
            patch(set_props(f"status:{i}", text=f"AI provider: {hint}", style="error"))
            continue
        state["proposals"][i] = {"message": message, "digest": change_digest(repo["path"])}
        patch(set_props(f"status:{i}", text=note, style="muted"),
              set_props(f"msg:{i}", value=message, visible=True))
    set_busy(False)
    if state["proposals"] and settings().get("skipReview"):
        commit_all()
    elif state["proposals"]:
        patch(set_props("commit", visible=True))


def commit_all():
    set_busy(True)
    include = settings().get("includeUntracked", True)
    done, failed = 0, 0
    for i, proposal in sorted(state["proposals"].items()):
        repo = state["repos"][i]
        if change_digest(repo["path"]) != proposal["digest"]:
            patch(set_props(f"status:{i}", text="Changed since the message was written · Flush again", style="error"))
            failed += 1
            continue
        message = proposal["message"].strip()
        if not message:
            patch(set_props(f"status:{i}", text="Empty message · skipped", style="error"))
            failed += 1
            continue
        # The upstream may have moved since the review (another repository in this list
        # can share it): bring the branch up to date again right before committing.
        patch(set_props(f"status:{i}", text="Checking upstream…", style="muted"))
        ok, note = bring_up_to_date(repo["path"])
        if ok:
            patch(set_props(f"status:{i}", text="Committing and pushing…"))
            ok, note = commit_and_push(repo["path"], message, include)
        patch(set_props(f"status:{i}", text=note, style="success" if ok else "error"),
              set_props(f"msg:{i}", enabled=False))
        done, failed = done + ok, failed + (not ok)
    state["proposals"] = {}
    patch(set_props("commit", visible=False))
    set_busy(False)
    refresh_badge(scan_all())
    plugin.call("ui.notify", title="Git Flush", body=f"{done} pushed, {failed} not pushed",
                urgency="normal" if not failed else "critical")


@plugin.on("view.closed")
def on_closed(handle):
    if handle == state["handle"]:
        state["handle"] = None


@plugin.on("overlay.shown")
def on_shown():
    refresh_badge(scan_all())


@plugin.on_activate
def start_polling(_info):
    def loop():
        while not plugin.stopping.is_set():
            try:
                refresh_badge(scan_all())
            except Exception as exc:  # keep polling; the log shows why
                plugin.log(f"poll failed: {exc}", "warn")
            plugin.stopping.wait(max(15, int(settings().get("pollSeconds", 60))))

    threading.Thread(target=loop, daemon=True).start()


plugin.run()
