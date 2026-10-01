#!/usr/bin/env python3
"""The Git Flush example plugin, end to end, on a real isolated daemon.

Real git repositories with a local bare remote; the AI provider is a stub
`claude` first on PATH (the real provider code runs; no account is used).
The panel is driven with `super-desktop plugin interact`, as an agent would.

Checks: dirty repositories listed; a diverged one and one whose change would
be overwritten are refused with their work kept; Flush all writes messages;
Commit & push pushes the others (one fast-forwarded first, one as a new
branch); no secret file content reaches the provider; the badge counts.
"""
import json
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plugin_isolated import ROOT, Isolated, wait_for  # noqa: E402

BIN = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/debug/super-desktop"
failures = []


def check(ok, what, detail=""):
    print(("ok   " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


def main():
    if not shutil.which("gtk4-broadwayd") or not BIN.exists():
        print("SKIP: needs gtk4-broadwayd and a built binary")
        return 0
    box = Isolated(BIN)
    env = box.env
    (box.home / ".gitconfig").write_text("[user]\n\tname = Test\n\temail = t@example.com\n[init]\n\tdefaultBranch = main\n[protocol \"file\"]\n\tallow = always\n")

    def sh(*args, cwd):
        result = subprocess.run(args, cwd=cwd, env=env, capture_output=True, text=True)
        assert result.returncode == 0, (args, result.stderr)
        return result.stdout.strip()

    repos = box.base / "repos"
    repos.mkdir()
    remote = repos / "remote.git"
    sh("git", "init", "--bare", "-q", str(remote), cwd=repos)
    seed = repos / "seed"
    sh("git", "clone", "-q", str(remote), str(seed), cwd=repos)
    for name in ("a.txt", "b.txt", "c.txt"):
        (seed / name).write_text("one\n")
    sh("git", "add", ".", cwd=seed)
    sh("git", "commit", "-qm", "init", cwd=seed)
    sh("git", "push", "-q", "origin", "main", cwd=seed)
    clones = {}
    for name in "ABDEG":
        clones[name] = repos / name
        sh("git", "clone", "-q", str(remote), str(clones[name]), cwd=repos)
    A, B, D, E, G = (clones[n] for n in "ABDEG")
    (A / "a.txt").write_text("one\ntwo from A\n")
    (A / ".env").write_text("TOKEN=supersecret\n")
    (seed / "b.txt").write_text("one\nremote change\n")
    sh("git", "commit", "-qam", "remote b", cwd=seed)
    sh("git", "push", "-q", cwd=seed)
    sh("git", "fetch", "-q", cwd=B)
    (B / "c.txt").write_text("one\nfrom B\n")
    (D / "a.txt").write_text("one\nlocal D\n")
    sh("git", "commit", "-qam", "local D", cwd=D)
    (D / "new.txt").write_text("x\n")
    sh("git", "checkout", "-qb", "feature", cwd=E)
    (E / "f.txt").write_text("feature\n")
    (G / "a.txt").write_text("one\nG edits the same line as A\n")

    prompts = box.base / "prompts.log"
    box.stub("claude", f"""#!/usr/bin/env python3
import json, sys
prompt = sys.stdin.read()
with open({str(prompts)!r}, "a") as f:
    f.write(json.dumps({{"args": sys.argv[1:], "prompt": prompt}}) + "\\n")
print(json.dumps({{"result": json.dumps({{"subject": "Update files", "body": "Written by the stub."}}), "is_error": False, "modelUsage": {{"stub-model": {{}}}}}}))
""")
    settings = box.home / ".config/super-desktop/plugins/git-flush/settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(json.dumps({"repositories": [str(clones[n]) for n in "ABDEG"], "pollSeconds": 3600}))

    try:
        check(box.start(), "isolated daemon is listening")
        plugin = box.copy_plugin(ROOT / "skills/super-desktop-plugin/examples/git-flush", "git-flush")
        check(box.cli("link", str(plugin), "--yes").returncode == 0, "link the example")
        check(box.cli("activate", "git-flush").returncode == 0, "activate")
        check(wait_for(lambda: box.status("git-flush")["state"] == "running") is not None, "running")
        check(box.cli("run", "git-flush", "git-flush.open").returncode == 0, "open the panel (as the shortcut does)")
        view = wait_for(lambda: box.views("git-flush")[0])
        nodes = view["nodes"] if view else {}
        rows = {n["label"]: key.split(":")[1] for key, n in nodes.items() if key.startswith("sel:")}
        check(set(rows) == {"A", "B", "D", "E", "G"}, "every dirty repository is listed", rows)

        def status(name):
            return (wait_for(lambda: box.views("git-flush")[0]) or {}).get("nodes", {}).get(f"status:{rows[name]}", {}).get("text", "")

        check(box.cli("interact", "git-flush", "flush", "click").returncode == 0, "click Flush all")
        ready = wait_for(lambda: box.views("git-flush")[0]["nodes"]["commit"]["visible"], timeout=60)
        check(ready is not None, "messages written; Commit & push is offered")
        check("diverged" in status("D"), "the diverged repository is refused", status("D"))
        check(status("B").startswith("will push to origin/main") and "remote change" in (B / "b.txt").read_text(), "B was fast-forwarded", status("B"))
        check(status("E") == "will create origin/feature", "a branch without upstream will be created", status("E"))
        message = box.views("git-flush")[0]["nodes"][f"msg:{rows['A']}"]
        check(message["visible"] and message["value"].startswith("Update files"), "the proposed message is shown for review", message)
        sent = [json.loads(line) for line in prompts.read_text().splitlines()]
        check(len(sent) == 4, "one provider call per repository that can be flushed", len(sent))
        check(all("--tools" in s["args"] and "" in s["args"] for s in sent), "the provider runs with every tool disabled")
        check(not any("supersecret" in s["prompt"] for s in sent) and any(".env" in s["prompt"] for s in sent), "secret content never reaches the provider")
        log = subprocess.run(["git", "--git-dir", str(remote), "log", "--oneline", "--all"], env=env, capture_output=True, text=True).stdout
        check("Update files" not in log, "nothing is committed before Commit & push", log)

        # Edit A's message as the user would, then commit.
        check(box.cli("interact", "git-flush", f"msg:{rows['A']}", "change", json.dumps("Describe A's change")).returncode == 0, "edit a message")
        import time
        time.sleep(0.6)  # the text area reports changes after 300 ms idle
        check(box.cli("interact", "git-flush", "commit", "click").returncode == 0, "click Commit & push")
        done = wait_for(lambda: not box.views("git-flush")[0]["nodes"]["commit"]["visible"] and box.views("git-flush")[0]["nodes"]["flush"]["enabled"], timeout=60)
        check(done is not None, "the run finishes")
        log = subprocess.run(["git", "--git-dir", str(remote), "log", "--format=%s", "main"], env=env, capture_output=True, text=True).stdout
        check("Describe A's change" in log and log.count("Update files") == 1, "A (edited message) and B pushed to main", log)
        feature = subprocess.run(["git", "--git-dir", str(remote), "log", "--format=%s", "feature"], env=env, capture_output=True, text=True).stdout
        check("Update files" in feature, "E pushed as a new branch")
        check("local D" not in log, "D was not pushed")
        check("would be overwritten" in status("G") and "G edits" in (G / "a.txt").read_text(), "G refused before committing; its work kept", status("G"))
        check(box.cli("deactivate", "git-flush").returncode == 0, "deactivate")
        check(box.ipc({"op": "footprint", "id": "git-flush"}).get("footprint") == [], "nothing left")
    finally:
        if failures:
            print("daemon stderr:\n" + box.daemon_log())
            log = box.home / ".local/state/super-desktop/plugins/git-flush/plugin.log"
            if log.exists():
                print("plugin log:\n" + log.read_text()[-4000:])
        box.close()
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
