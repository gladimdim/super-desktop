"""Run rebuild.sh against a tiny Rust project, without touching the desktop."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REBUILD = Path(__file__).resolve().parents[1] / "rebuild.sh"


@unittest.skipUnless(shutil.which("cargo"), "cargo is required")
class RebuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sd-rebuild-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.project = self.root / "source"
        self.project.mkdir()
        shutil.copy2(REBUILD, self.project / "rebuild.sh")
        (self.project / "Cargo.toml").write_text(
            '[package]\nname = "super-desktop"\nversion = "0.1.0"\nedition = "2021"\n'
        )
        (self.project / "src" / "bin").mkdir(parents=True)
        self.write_binaries("current")
        self.environment = dict(os.environ, CARGO_TARGET_DIR=str(self.root / "external-target"))

    def write_binaries(self, message):
        for name in ("super-desktop", "super-desktop-client"):
            (self.project / "src" / "bin" / f"{name}.rs").write_text(
                f'fn main() {{ println!("{message}"); }}\n'
            )

    def rebuild(self, *arguments):
        result = subprocess.run(
            ["bash", str(self.project / "rebuild.sh"), "--no-daemon", *arguments],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for name in ("super-desktop", "super-desktop-client"):
            binary = self.project / "target" / "release" / name
            self.assertTrue(binary.is_file(), f"Missing installed-path binary: {binary}")
            self.assertEqual(subprocess.check_output([str(binary)], text=True).strip(), "current")
        self.assertFalse((self.root / "external-target").exists())

    def test_fresh_build_ignores_inherited_target_directory(self):
        self.rebuild()

    def test_clean_rebuild_replaces_existing_binaries(self):
        self.write_binaries("old")
        subprocess.run(
            ["cargo", "build", "--release", "--manifest-path", str(self.project / "Cargo.toml")],
            cwd=self.root,
            env=dict(self.environment, CARGO_TARGET_DIR=str(self.project / "target")),
            check=True,
            capture_output=True,
            timeout=60,
        )
        self.write_binaries("current")
        self.rebuild("--clean")


if __name__ == "__main__":
    unittest.main()
