"""The patterns rebuild.sh, install.sh and the bridge use to find SUPER DESKTOP's
own processes match those processes, and never a process whose command text
merely mentions them (an agent's shell command in a card).

Only `pgrep` runs here, and only the processes this test starts are checked.
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]


def shell_pattern(path, name):
    match = re.search(rf"^{name}='([^']+)'$", (ROOT / path).read_text(), re.M)
    if not match:
        raise AssertionError(f"{path} defines no {name}")
    return match.group(1)


def rust_pattern(path, name):
    match = re.search(rf'^const {name}: &str = "([^"]+)";$', (ROOT / path).read_text(), re.M)
    if not match:
        raise AssertionError(f"{path} defines no {name}")
    return match.group(1)


@unittest.skipUnless(shutil.which("pgrep"), "pgrep is required")
class ProcessPatternTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sd-patterns-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.processes = []

    def tearDown(self):
        for process in self.processes:
            process.kill()
            process.wait()

    def start(self, argv):
        """A sleeping process whose command line is exactly `argv`."""
        # Python runs the file named by argv[1], relative to cwd.
        (self.root / argv[1]).write_text("import time\ntime.sleep(60)\n")
        process = subprocess.Popen(argv, executable=sys.executable, cwd=self.root)
        self.processes.append(process)
        return process.pid

    def start_decoy(self, text):
        process = subprocess.Popen(["bash", "-c", f"sleep 60; : {text}"])
        self.processes.append(process)
        return process.pid

    def matches(self, pattern):
        result = subprocess.run(["pgrep", "-f", pattern], capture_output=True, text=True)
        return {int(pid) for pid in result.stdout.split()}

    def settle(self, pids):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if all(Path(f"/proc/{pid}/cmdline").read_bytes() for pid in pids):
                return
            time.sleep(0.05)

    def test_daemon_pattern_is_the_same_in_both_scripts(self):
        self.assertEqual(
            shell_pattern("rebuild.sh", "DAEMON_PROCESS"),
            shell_pattern("install.sh", "DAEMON_PROCESS"),
        )

    def test_bridge_pattern_is_the_same_in_rebuild_and_the_bridge(self):
        self.assertEqual(
            shell_pattern("rebuild.sh", "BRIDGE_PROCESS"),
            rust_pattern("src/bridge.rs", "BRIDGE_PROCESS"),
        )

    def test_daemon_pattern_finds_daemons_and_spares_mentions(self):
        pattern = shell_pattern("rebuild.sh", "DAEMON_PROCESS")
        daemons = [
            self.start([str(self.root / "target/release/super-desktop"), "daemon"]),
            self.start(["super-desktop", "daemon"]),
        ]
        others = [
            self.start_decoy("cd ~/GitHub/super-desktop && grep daemon rebuild.sh"),
            self.start_decoy("super-desktop daemon"),
            self.start([str(self.root / "super-desktop-client"), "daemon"]),
            self.start([str(self.root / "super-desktop"), "daemonize"]),
        ]
        self.settle(daemons + others)
        found = self.matches(pattern)
        for pid in daemons:
            self.assertIn(pid, found)
        for pid in others:
            self.assertNotIn(pid, found)

    def test_bridge_pattern_finds_the_bridge_and_spares_mentions(self):
        pattern = shell_pattern("rebuild.sh", "BRIDGE_PROCESS")
        bridges = [
            self.start(["super-desktop", "harness-bridge"]),
            self.start([str(self.root / "bin/super-desktop"), "harness-bridge"]),
        ]
        others = [
            self.start_decoy("pkill -f 'super-desktop harness-bridge'"),
            self.start([str(self.root / "super-desktop"), "harness-event"]),
        ]
        self.settle(bridges + others)
        found = self.matches(pattern)
        for pid in bridges:
            self.assertIn(pid, found)
        for pid in others:
            self.assertNotIn(pid, found)


if __name__ == "__main__":
    unittest.main()
