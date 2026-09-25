#!/usr/bin/env python3
"""Two-PC release measurements.

Measures, between a viewer PC (A) and a host PC (B):
  * echo latency: a key typed on A travels to B's shell and its echo comes back
    (min/p50/p95/p99/max over many keystrokes);
  * idle cost of 1, 4 and 8 live consoles: CPU and network on both PCs;
  * repeated attach/detach cycles: B's bridge memory, file descriptors, threads
    and tmux client count must stay bounded.

Usage (both PCs on the same build; B's overlay may stay hidden):

  On B (host), start the sampler first and leave it running:
      python3 tests/two_pc_measure.py sample --peer-ip A_IP --out host.jsonl
  On A (viewer), with A's overlay hidden or on "This PC":
      super-desktop peer-list                      # find B's machineId
      python3 tests/two_pc_measure.py viewer MACHINE_ID --out viewer.json
  Stop the sampler on B (Ctrl+C), copy host.jsonl to A, then:
      python3 tests/two_pc_measure.py report viewer.json host.jsonl

The viewer creates its own Shell cards on B (B's top bar must offer Shell),
types only letters and Ctrl+U into them (never Enter), and closes them at the
end, also after Ctrl+C. It never touches existing cards. If a run is killed
hard, `python3 tests/two_pc_measure.py cleanup MACHINE_ID` closes leftovers.

`python3 tests/two_pc_measure.py simulate` runs the whole pipeline on this
machine against the simulated host of tests/two_pc_matrix.py (isolated bridge,
private tmux server), to validate the tool itself. Its numbers are loopback
numbers, not a release measurement.
"""
import argparse
import collections
import json
import os
import pathlib
import signal
import statistics
import subprocess
import sys
import threading
import time

REPO = pathlib.Path(__file__).resolve().parent.parent
CLK_TCK = os.sysconf("SC_CLK_TCK")
STATE_FILE = pathlib.Path.home() / ".cache/super-desktop/two_pc_measure_cards.json"
STARTED = time.monotonic()


def log(message):
    print(f"[{time.monotonic() - STARTED:6.1f}s] {message}", file=sys.stderr, flush=True)


def percentile(values, fraction):
    ordered = sorted(values)
    if not ordered:
        return None
    index = min(len(ordered) - 1, max(0, round(fraction * (len(ordered) - 1))))
    return ordered[index]


# --------------------------------------------------------------------------
# /proc sampling
# --------------------------------------------------------------------------

def read_procs():
    """pid -> (ppid, own cpu ticks, reaped-children cpu ticks, argv)."""
    procs = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as handle:
                stat = handle.read()
            with open(f"/proc/{entry}/cmdline", "rb") as handle:
                argv = [part.decode(errors="replace") for part in handle.read().split(b"\0") if part]
        except OSError:
            continue
        fields = stat[stat.rfind(")") + 2:].split()
        # fields[0] is state (field 3); utime=14, stime=15, cutime=16, cstime=17.
        procs[int(entry)] = (int(fields[1]), int(fields[11]) + int(fields[12]),
                             int(fields[13]) + int(fields[14]), argv)
    return procs


def subtree_ticks(procs, root):
    """CPU ticks of `root`, its live descendants and the children it reaped."""
    if root not in procs:
        return None
    children = collections.defaultdict(list)
    for pid, (ppid, *_rest) in procs.items():
        children[ppid].append(pid)
    total = procs[root][2]
    stack = [root]
    while stack:
        pid = stack.pop()
        total += procs[pid][1]
        stack.extend(children.get(pid, ()))
    return total


def status_fields(pid):
    out = {}
    try:
        with open(f"/proc/{pid}/status") as handle:
            for line in handle:
                key, _, value = line.partition(":")
                if key in ("VmRSS", "Threads"):
                    out[key] = int(value.split()[0])
        out["fds"] = len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        return None
    return out


def net_bytes():
    rx = tx = 0
    with open("/proc/net/dev") as handle:
        for line in handle.readlines()[2:]:
            name, _, data = line.partition(":")
            if name.strip() == "lo":
                continue
            fields = data.split()
            rx += int(fields[0])
            tx += int(fields[8])
    return rx, tx


def tcp_bytes(local_port=None, remote_port=None, remote_ip=None):
    """(sent, received) over established TCP connections matching the filter,
    from the kernel's per-socket counters: only the traffic between the two
    PCs, not the browser or a phone on the same bridge."""
    selector = f"( sport = :{local_port} )" if local_port else f"( dport = :{remote_port} )"
    try:
        text = subprocess.run(["ss", "-tinH", "state", "established", selector],
                              capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    sent = received = 0
    lines = text.splitlines()
    for header, info in zip(lines[::2], lines[1::2]):
        peer = header.split()[-1].rsplit(":", 1)[0].strip("[]")
        if remote_ip and peer != remote_ip:
            continue
        fields = dict(item.split(":", 1) for item in info.split() if ":" in item)
        sent += int(fields.get("bytes_acked", fields.get("bytes_sent", 0)))
        received += int(fields.get("bytes_received", 0))
    return sent, received


def find_role(procs, role):
    """The desktop daemon (`daemon`) or bridge (`harness-bridge`) process."""
    for pid, (_ppid, _own, _reaped, argv) in procs.items():
        if len(argv) >= 2 and os.path.basename(argv[0]) == "super-desktop" and argv[1] == role:
            return pid
    return None


def tmux_facts(env=None):
    def run(*args):
        try:
            return subprocess.run(["tmux", *args], capture_output=True, text=True,
                                  timeout=5, env=env).stdout
        except (OSError, subprocess.TimeoutExpired):
            return ""
    server = run("list-sessions", "-F", "#{pid}").split()
    clients = [line for line in run("list-clients", "-F", "#{client_name}").splitlines() if line]
    return (int(server[0]) if server else None), len(clients)


class Sampler:
    """One JSON line per second: cumulative CPU ticks per role, memory, fds,
    threads, tmux clients and interface bytes. Summaries use deltas."""

    def __init__(self, out, roles=None, tmux_env=None, peer_ip=None, port=8759):
        self.out = out
        self.peer_ip = peer_ip
        self.port = port
        self.roles = roles  # {"bridge": pid, ...} fixed PIDs (simulate), else discovered
        self.tmux_env = tmux_env
        self.stopped = threading.Event()

    def sample(self):
        procs = read_procs()
        roles = dict(self.roles) if self.roles else {
            "daemon": find_role(procs, "daemon"), "bridge": find_role(procs, "harness-bridge")}
        server, clients = tmux_facts(self.tmux_env)
        roles["tmux"] = server
        row = {"t": time.time(), "cpu": {}, "proc": {}, "clients": clients}
        for role, pid in roles.items():
            if pid is None:
                continue
            row["cpu"][role] = subtree_ticks(procs, pid) if role != "tmux" else \
                (procs[pid][1] if pid in procs else None)
            if role != "tmux":
                row["proc"][role] = status_fields(pid)
        row["net"] = net_bytes()
        row["tcp"] = tcp_bytes(local_port=self.port, remote_ip=self.peer_ip)
        return row

    def run(self, seconds=None):
        deadline = time.time() + seconds if seconds else None
        with open(self.out, "a") as handle:
            while not self.stopped.is_set() and (deadline is None or time.time() < deadline):
                handle.write(json.dumps(self.sample()) + "\n")
                handle.flush()
                self.stopped.wait(1.0)


# --------------------------------------------------------------------------
# The viewer side: real CLI against a paired PC
# --------------------------------------------------------------------------

class Cli:
    def __init__(self, binary, env=None):
        self.binary = binary
        self.env = env

    def run(self, *args, input=None, timeout=30):
        return subprocess.run([self.binary, *args], input=input, env=self.env,
                              capture_output=True, text=True, timeout=timeout)

    def json(self, *args, input=None):
        result = self.run(*args, input=input)
        if not result.stdout.strip():
            raise SystemExit(f"{args[0]} failed: {result.stderr.strip() or result.returncode}")
        return json.loads(result.stdout)

    def popen(self, *args, stdin):
        return subprocess.Popen([self.binary, *args], env=self.env, stdin=stdin,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)


class Attach:
    """A running `peer-attach`: output bytes timestamped, "attached" watched."""

    def __init__(self, cli, machine, card, typing):
        self.process = cli.popen("peer-attach", machine, card,
                                 stdin=subprocess.PIPE if typing else subprocess.DEVNULL)
        self.started = time.monotonic()
        self.attached_at = None
        self.last_output = time.monotonic()
        self.output_events = collections.deque(maxlen=4096)
        self.lock = threading.Condition()
        self.error = []
        threading.Thread(target=self._read_out, daemon=True).start()
        threading.Thread(target=self._read_err, daemon=True).start()

    def _read_out(self):
        fd = self.process.stdout.fileno()
        while True:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                return
            if not chunk:
                return
            now = time.monotonic()
            with self.lock:
                self.last_output = now
                self.output_events.append(now)
                self.lock.notify_all()

    def _read_err(self):
        for line in iter(self.process.stderr.readline, b""):
            text = line.decode(errors="replace").strip()
            with self.lock:
                if text.startswith("attached") and self.attached_at is None:
                    self.attached_at = time.monotonic()
                    self.lock.notify_all()
                else:
                    self.error.append(text)

    def wait_attached(self, timeout=20):
        with self.lock:
            self.lock.wait_for(lambda: self.attached_at is not None
                               or self.process.poll() is not None, timeout)
            if self.attached_at is None:
                raise RuntimeError(f"attach failed: {' | '.join(self.error[-3:]) or 'timeout'}")
            return self.attached_at - self.started

    def wait_quiet(self, quiet=0.15, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            with self.lock:
                idle = time.monotonic() - self.last_output
            if idle >= quiet:
                return True
            time.sleep(min(quiet - idle, 0.05) + 0.001)
        return False

    def type_and_time(self, data, timeout=2.0):
        """Seconds until the first output byte after `data` is written."""
        sent = time.monotonic()
        self.process.stdin.write(data)
        self.process.stdin.flush()
        with self.lock:
            got = self.lock.wait_for(lambda: self.output_events and self.output_events[-1] > sent,
                                     timeout)
            if not got:
                return None
            first = next(t for t in self.output_events if t > sent)
        return first - sent

    def cpu_ticks(self):
        try:
            with open(f"/proc/{self.process.pid}/stat") as handle:
                fields = handle.read().rsplit(")", 1)[1].split()
            return int(fields[11]) + int(fields[12])
        except OSError:
            return 0

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)


def create_cards(cli, machine, count):
    snapshot = cli.json("peer-workspace", machine)
    if "shell" not in snapshot.get("visibleHarnesses", []):
        raise SystemExit("B's top bar does not offer Shell: enable it in B's Settings "
                         "(harness list) and run again.")
    created = []
    for index in range(count):
        body = {"command": {"type": "createTerminal", "agentType": "shell",
                            "workspace": snapshot["workspace"]}}
        answer = cli.json("peer-command", machine, input=json.dumps(body))
        result = (answer.get("reply") or {}).get("result") or {}
        if result.get("type") != "applied" or not result.get("cardId"):
            raise SystemExit(f"could not create measurement card {index + 1}: "
                             f"{answer.get('error') or result}")
        created.append(result["cardId"])
        remember(machine, created)
        log(f"created measurement card {index + 1}/{count}: {result['cardId']}")
    return created


def remember(machine, cards):
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    STATE_FILE.write_text(json.dumps({"machine": machine, "cards": cards}))


def close_cards(cli, machine, cards):
    for card_id in cards:
        try:
            snapshot = cli.json("peer-workspace", machine)
            card = next((c for c in snapshot["cards"] if c["cardId"] == card_id), None)
            if card is None:
                continue
            body = {"command": {"type": "closeTerminal", "cardId": card_id,
                                "expectedRevision": card["revision"]}}
            answer = cli.json("peer-command", machine, input=json.dumps(body))
            log(f"closed {card_id}: {answer.get('notice') or 'ok'}")
        except (SystemExit, Exception) as error:  # cleanup must try every card
            log(f"could not close {card_id}: {error}")
    STATE_FILE.unlink(missing_ok=True)


def measure_echo(cli, machine, card, samples):
    attach = Attach(cli, machine, card, typing=True)
    try:
        attach_seconds = attach.wait_attached()
        time.sleep(1.5)
        attach.wait_quiet(0.5, 10)
        latencies, timeouts = [], 0
        letters = b"abcdefghijklmnopqrstuvwxyz"
        for index in range(samples):
            attach.wait_quiet(0.15, 5)
            latency = attach.type_and_time(letters[index % 26:index % 26 + 1])
            if latency is None:
                timeouts += 1
            else:
                latencies.append(latency * 1000)
            if index % 15 == 14:
                attach.wait_quiet(0.15, 5)
                attach.type_and_time(b"\x15")  # Ctrl+U: clear the typed line
        attach.wait_quiet(0.15, 5)
        attach.type_and_time(b"\x15")
    finally:
        attach.stop()
    return {
        "samples": len(latencies), "timeouts": timeouts,
        "attachMs": round(attach_seconds * 1000, 1),
        "minMs": round(min(latencies), 1) if latencies else None,
        "p50Ms": round(percentile(latencies, 0.50), 1) if latencies else None,
        "p95Ms": round(percentile(latencies, 0.95), 1) if latencies else None,
        "p99Ms": round(percentile(latencies, 0.99), 1) if latencies else None,
        "maxMs": round(max(latencies), 1) if latencies else None,
        "meanMs": round(statistics.fmean(latencies), 1) if latencies else None,
    }


def viewer(args, cli=None, on_phase=None):
    cli = cli or Cli(args.binary)
    consoles = sorted({int(n) for n in args.consoles.split(",")})
    if max(consoles) > 8:
        raise SystemExit("a host serves at most 8 attachments per credential")
    phases = []
    report = {"machine": args.machine, "started": time.time(),
              "consoles": consoles, "phases": phases}
    peers = cli.json("peer-list")
    peer = next((p for p in peers if p["machineId"] == args.machine), None)
    if peer is None:
        raise SystemExit(f"{args.machine} is not a paired PC (see `super-desktop peer-list`)")
    report["host"] = {"label": peer.get("label"), "endpoint": peer.get("endpoint")}
    try:
        route = subprocess.run(["ip", "route", "get", peer["endpoint"]["host"]],
                               capture_output=True, text=True).stdout.split("\n")[0]
    except OSError:
        route = ""
    report["route"] = route
    host_ip, port = peer["endpoint"]["host"], peer["endpoint"]["port"]

    def phase(name, **extra):
        entry = {"name": name, "start": time.time(), **extra}
        phases.append(entry)
        if on_phase:
            on_phase(name)
        return entry

    created = []
    try:
        created = create_cards(cli, args.machine, max(consoles))
        time.sleep(2)

        log(f"echo: {args.echo} keystrokes into {created[0]}")
        entry = phase("echo")
        report["echo"] = measure_echo(cli, args.machine, created[0], args.echo)
        entry["end"] = time.time()
        log(f"echo: {report['echo']}")

        report["idle"] = {}
        for count in consoles:
            attaches = [Attach(cli, args.machine, card, typing=False) for card in created[:count]]
            try:
                for attach in attaches:
                    attach.wait_attached()
                time.sleep(5)
                ticks0 = sum(a.cpu_ticks() for a in attaches)
                net0 = tcp_bytes(remote_port=port, remote_ip=host_ip) or (0, 0)
                entry = phase(f"idle-{count}", consoles=count)
                time.sleep(args.idle)
                entry["end"] = time.time()
                ticks1 = sum(a.cpu_ticks() for a in attaches)
                net1 = tcp_bytes(remote_port=port, remote_ip=host_ip) or (0, 0)
            finally:
                for attach in attaches:
                    attach.stop()
            seconds = entry["end"] - entry["start"]
            report["idle"][count] = {
                "viewerCliCpuPct": round((ticks1 - ticks0) / CLK_TCK / seconds * 100, 2),
                # From the viewer's side: received from B, sent to B.
                "viewerRxBps": round((net1[1] - net0[1]) / seconds),
                "viewerTxBps": round((net1[0] - net0[0]) / seconds),
            }
            log(f"idle with {count} console(s): {report['idle'][count]}")
            time.sleep(3)

        entry = phase("before-cycles")
        time.sleep(10)
        entry["end"] = time.time()
        count = max(consoles)
        log(f"cycles: {args.cycles} x attach/detach of {count} console(s)")
        entry = phase("cycles", consoles=count)
        attach_times, failures = [], 0
        for _cycle in range(args.cycles):
            attaches = [Attach(cli, args.machine, card, typing=False) for card in created[:count]]
            for attach in attaches:
                try:
                    attach_times.append(attach.wait_attached() * 1000)
                except RuntimeError:
                    failures += 1
            for attach in attaches:
                attach.stop()
        entry["end"] = time.time()
        time.sleep(10)
        entry = phase("after-cycles")
        time.sleep(10)
        entry["end"] = time.time()
        report["cycles"] = {"cycles": args.cycles, "consoles": count, "failures": failures,
                            "attachP50Ms": round(percentile(attach_times, 0.5), 1) if attach_times else None,
                            "attachP95Ms": round(percentile(attach_times, 0.95), 1) if attach_times else None}
        log(f"cycles: {report['cycles']}")
    finally:
        close_cards(cli, args.machine, created)
        report["finished"] = time.time()
        pathlib.Path(args.out).write_text(json.dumps(report, indent=2))
        log(f"wrote {args.out}")
    return report


# --------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------

def host_window(rows, start, end):
    inside = [row for row in rows if start <= row["t"] <= end]
    if len(inside) < 2:
        return None
    first, last = inside[0], inside[-1]
    seconds = last["t"] - first["t"]
    out = {"seconds": round(seconds, 1), "cpuPct": {}}
    for role in ("daemon", "bridge", "tmux"):
        a, b = first["cpu"].get(role), last["cpu"].get(role)
        if a is not None and b is not None:
            out["cpuPct"][role] = round((b - a) / CLK_TCK / seconds * 100, 2)
    if first.get("tcp") and last.get("tcp"):
        # Bridge sockets toward the viewer: tx = sent to A, rx = received from A.
        out["txBps"] = round((last["tcp"][0] - first["tcp"][0]) / seconds)
        out["rxBps"] = round((last["tcp"][1] - first["tcp"][1]) / seconds)
    else:
        out["rxBps"] = round((last["net"][0] - first["net"][0]) / seconds)
        out["txBps"] = round((last["net"][1] - first["net"][1]) / seconds)
    out["clientsMax"] = max(row["clients"] for row in inside)
    for role in ("bridge", "daemon"):
        values = [row["proc"].get(role) for row in inside if row["proc"].get(role)]
        if values:
            out[role] = {"rssKbLast": values[-1]["VmRSS"], "fdsLast": values[-1]["fds"],
                         "threadsLast": values[-1]["Threads"],
                         "rssKbMax": max(v["VmRSS"] for v in values),
                         "fdsMax": max(v["fds"] for v in values)}
    out["clientsLast"] = last["clients"]
    return out


def report(args):
    view = json.loads(pathlib.Path(args.viewer).read_text())
    rows = [json.loads(line) for line in pathlib.Path(args.host).read_text().splitlines() if line]
    windows = {p["name"]: host_window(rows, p["start"], p["end"]) for p in view["phases"] if "end" in p}
    echo = view.get("echo", {})
    lines = [
        f"## Two-PC measurements: viewer → {view['host'].get('label')} "
        f"({view['host']['endpoint']['host']})",
        "",
        f"Route: `{view.get('route') or 'unknown'}`",
        "",
        "### Echo latency (keystroke on A → echo from B's shell, CLI viewer)",
        "",
        "| samples | timeouts | min | p50 | p95 | p99 | max | attach |",
        "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
        f"| {echo.get('samples')} | {echo.get('timeouts')} | {echo.get('minMs')} ms | "
        f"{echo.get('p50Ms')} ms | **{echo.get('p95Ms')} ms** | {echo.get('p99Ms')} ms | "
        f"{echo.get('maxMs')} ms | {echo.get('attachMs')} ms |",
        "",
        f"Target: p95 below 100 ms on an unloaded wired LAN → "
        f"{'met' if (echo.get('p95Ms') or 1e9) < 100 else 'NOT met'}.",
        "",
        "### Idle cost per number of live consoles",
        "",
        "| consoles | A: viewer CPU | A: rx/tx B/s | B: bridge CPU | B: tmux CPU | B: daemon CPU | B: rx/tx B/s | B: tmux clients |",
        "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for count, idle in view.get("idle", {}).items():
        host = windows.get(f"idle-{count}") or {}
        cpu = host.get("cpuPct", {})
        lines.append(
            f"| {count} | {idle['viewerCliCpuPct']}% | {idle['viewerRxBps']}/{idle['viewerTxBps']} | "
            f"{cpu.get('bridge', '–')}% | {cpu.get('tmux', '–')}% | {cpu.get('daemon', '–')}% | "
            f"{host.get('rxBps', '–')}/{host.get('txBps', '–')} | {host.get('clientsMax', '–')} |")
    before, after = windows.get("before-cycles") or {}, windows.get("after-cycles") or {}
    cycles = view.get("cycles", {})
    lines += ["", f"### Leak check: {cycles.get('cycles')} attach/detach cycles of "
                  f"{cycles.get('consoles')} consoles ({cycles.get('failures')} failed; attach "
                  f"p50 {cycles.get('attachP50Ms')} ms, p95 {cycles.get('attachP95Ms')} ms)", "",
              "| B | before | after |", "| --- | ---: | ---: |"]
    for label, key in (("bridge RSS (KiB)", "rssKbLast"), ("bridge fds", "fdsLast"),
                       ("bridge threads", "threadsLast")):
        lines.append(f"| {label} | {(before.get('bridge') or {}).get(key, '–')} | "
                     f"{(after.get('bridge') or {}).get(key, '–')} |")
    lines.append(f"| tmux clients | {before.get('clientsLast', '–')} | {after.get('clientsLast', '–')} |")
    lines += ["", "B's CPU is per role: the bridge (with its attach clients), the tmux server and the "
                  "desktop daemon. The CLI viewer excludes A's GTK rendering; measure that "
                  "separately with `sample` on A while the overlay shows B."]
    print("\n".join(lines))


# --------------------------------------------------------------------------
# Self-test against the simulated host of two_pc_matrix.py
# --------------------------------------------------------------------------

def simulate(args):
    import tempfile
    sys.path.insert(0, str(REPO / "tests"))
    import two_pc_matrix as matrix
    subprocess.run(["cargo", "build", "--bin", "super-desktop"], cwd=REPO, check=True)
    binary = str(REPO / "target/debug/super-desktop")
    root = pathlib.Path(tempfile.mkdtemp(prefix="sdmeasure-", dir="/tmp"))
    os.chmod(root, 0o700)
    global STATE_FILE
    STATE_FILE = root / "cards.json"
    host = viewer_pc = sampler = None
    try:
        host = matrix.Host(binary, root, "b", matrix.seed_cards("b", 1))
        viewer_pc = matrix.Viewer(binary, root, "a")
        machine = viewer_pc.pair(host)
        sampler = Sampler(str(root / "host.jsonl"), roles={"bridge": host.bridge.pid},
                          tmux_env=host.env, peer_ip="127.0.0.1", port=host.port)
        thread = threading.Thread(target=sampler.run, daemon=True)
        thread.start()
        view_args = argparse.Namespace(machine=machine, consoles="1,2", echo=60, idle=4,
                                       cycles=4, out=str(root / "viewer.json"), binary=binary)
        result = viewer(view_args, cli=Cli(binary, viewer_pc.env))
        sampler.stopped.set()
        thread.join(5)
        assert result["echo"]["samples"] >= 50, result["echo"]
        assert set(result["idle"]) == {1, 2}, result["idle"]
        assert result["cycles"]["failures"] == 0, result["cycles"]
        left = [c for c in host.owner.snapshot()["cards"] if c["cardId"].startswith("b-new")]
        assert not left, f"measurement cards were not closed: {left}"
        report(argparse.Namespace(viewer=view_args.out, host=str(root / "host.jsonl")))
        log("simulate: ok (loopback numbers; not a release measurement)")
    finally:
        if sampler:
            sampler.stopped.set()
        for process in reversed(matrix.PROCESSES):
            matrix.stop(process, timeout=5)
        if host:
            host.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="mode", required=True)
    s = sub.add_parser("sample", help="sample this PC's daemon, bridge and tmux (run on B)")
    s.add_argument("--out", default="host.jsonl")
    s.add_argument("--seconds", type=int)
    s.add_argument("--peer-ip", help="count only bridge traffic with the viewer PC (its IP)")
    v = sub.add_parser("viewer", help="measure from this PC against a paired PC (run on A)")
    v.add_argument("machine")
    v.add_argument("--out", default="viewer.json")
    v.add_argument("--consoles", default="1,4,8")
    v.add_argument("--echo", type=int, default=300, help="keystrokes to time")
    v.add_argument("--idle", type=int, default=60, help="seconds per idle window")
    v.add_argument("--cycles", type=int, default=30)
    v.add_argument("--binary", default="super-desktop")
    r = sub.add_parser("report", help="combine viewer.json and host.jsonl as markdown")
    r.add_argument("viewer")
    r.add_argument("host")
    c = sub.add_parser("cleanup", help="close measurement cards left by a killed run")
    c.add_argument("machine")
    c.add_argument("--binary", default="super-desktop")
    sub.add_parser("simulate", help="validate this tool on one machine")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    if args.mode == "sample":
        log(f"sampling every second into {args.out}; Ctrl+C to stop")
        try:
            Sampler(args.out, peer_ip=args.peer_ip).run(args.seconds)
        except KeyboardInterrupt:
            pass
    elif args.mode == "viewer":
        viewer(args)
    elif args.mode == "report":
        report(args)
    elif args.mode == "cleanup":
        state = json.loads(STATE_FILE.read_text()) if STATE_FILE.exists() else {}
        if state.get("machine") != args.machine:
            raise SystemExit("no recorded measurement cards for that PC")
        close_cards(Cli(args.binary), args.machine, state["cards"])
    else:
        simulate(args)


if __name__ == "__main__":
    main()
