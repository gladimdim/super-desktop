import contextlib
import io
import hashlib
import json
import os
from pathlib import Path
import runpy
import subprocess
import tempfile
import unittest
import shutil
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
SETUP = runpy.run_path(str(ROOT / "packaging/arch/super-desktop-setup"))
PACKAGE = runpy.run_path(str(ROOT / "scripts/package-arch.py"))
SOURCES = runpy.run_path(str(ROOT / "scripts/arch_sources.py"))
ASSETS = runpy.run_path(str(ROOT / "scripts/arch_assets.py"))


class DesktopSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.hypr = self.home / ".config/hypr"
        self.hypr.mkdir(parents=True)
        (self.hypr / "hyprland.lua").write_text('-- user configuration\n')

    def configure(self, migrate=False):
        with contextlib.redirect_stdout(io.StringIO()):
            SETUP["configure"](self.home, migrate)

    def snapshot(self):
        return {str(p.relative_to(self.home)): p.read_bytes() for p in self.home.rglob("*") if p.is_file()}

    def test_fresh_setup_is_idempotent_and_backs_up_configuration(self):
        self.configure()
        first = self.snapshot()
        self.configure()
        self.assertEqual(first, self.snapshot())
        self.assertIn("super-desktop toggle", (self.hypr / "bindings.lua").read_text())
        self.assertEqual((self.hypr / "hyprland.lua.before-package").read_text(), '-- user configuration\n')
        hook = self.home / ".config/omarchy/hooks/theme-set.d/super-desktop"
        self.assertTrue(os.access(hook, os.X_OK))

    def test_custom_shortcut_and_hook_survive_setup(self):
        shortcut = 'o.bind("SUPER + A", "Custom", "super-desktop toggle")\n'
        (self.hypr / "bindings.lua").write_text(shortcut)
        hook = self.home / ".config/omarchy/hooks/theme-set.d/super-desktop"
        hook.parent.mkdir(parents=True)
        hook.write_text("custom hook\n")
        self.configure()
        self.assertEqual((self.hypr / "bindings.lua").read_text(), shortcut)
        self.assertEqual(hook.read_text(), "custom hook\n")

    def test_migration_requires_flag_then_preserves_original_symlink(self):
        launcher = self.home / ".local/bin/super-desktop"
        launcher.parent.mkdir(parents=True)
        original = "/missing/source/target/release/super-desktop-client"
        launcher.symlink_to(original)
        desktop = self.home / ".local/share/applications/super-desktop.desktop"
        desktop.parent.mkdir(parents=True)
        desktop.write_text("[Desktop Entry]\nExec=/old/.local/bin/super-desktop toggle\n")
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, "--migrate"):
            self.configure()
        self.assertEqual(before, self.snapshot())
        self.configure(migrate=True)
        self.assertEqual(os.readlink(launcher), "/usr/bin/super-desktop")
        self.assertEqual(os.readlink(launcher.with_name("super-desktop.before-package")), original)
        self.assertFalse(desktop.exists())
        self.assertTrue(desktop.with_name("super-desktop.desktop.before-package").is_file())
        self.configure(migrate=True)

    def test_custom_launcher_is_never_overwritten(self):
        launcher = self.home / ".local/bin/super-desktop"
        launcher.parent.mkdir(parents=True)
        launcher.write_text("my custom launcher")
        with self.assertRaisesRegex(ValueError, "Custom launcher"):
            self.configure(migrate=True)
        self.assertEqual(launcher.read_text(), "my custom launcher")


class PackageTests(unittest.TestCase):
    def test_exported_artwork_replaces_restricted_marks_and_removes_stale_assets(self):
        original = json.loads((ROOT / "assets/logos/harness-logos.json").read_text())
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            directory = root / "assets/logos"
            shutil.copytree(ROOT / "assets/logos", directory)
            ASSETS["prepare_assets"](root)
            packaged = json.loads((directory / "harness-logos.json").read_text())
            self.assertEqual(set(packaged), set(original))
            for key, entry in packaged.items():
                for name, digest in entry["files"].items():
                    data = (directory / name).read_bytes()
                    self.assertEqual(hashlib.sha256(data).hexdigest(), digest)
                    if key in ASSETS["INITIALS"]:
                        ET.fromstring(data)
                        self.assertNotEqual(data, (ROOT / "assets/logos" / name).read_bytes())
                if key not in ASSETS["INITIALS"]:
                    self.assertEqual(entry, original[key])
            self.assertFalse((directory / "anthropic-black.svg").exists())
            self.assertFalse((directory / "google.svg").exists())
            snapshot = {p.name: p.read_bytes() for p in directory.iterdir()}
            ASSETS["prepare_assets"](root)
            self.assertEqual(snapshot, {p.name: p.read_bytes() for p in directory.iterdir()})
            (root / ".git").mkdir()
            with self.assertRaisesRegex(ValueError, "exported source"):
                ASSETS["prepare_assets"](root)

    def test_undeclared_license_blocks_distribution(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "Cargo.toml").write_text('[package]\nversion="1.2.3"\n')
            with self.assertRaisesRegex(ValueError, "license"):
                PACKAGE["release_metadata"](root)

    def test_stage_keeps_client_and_application_siblings(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            for relative in ("LICENSE", "super-desktop.desktop", "packaging/arch/super-desktop-setup", "assets/logos/LICENSES.md", "assets/logos/ATTRIBUTION.md", "assets/icons/hicolor/test.svg", "bin/super-desktop", "bin/super-desktop-client"):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture\n")
            staged = Path(temp) / "staged"
            PACKAGE["stage"](root, root / "bin", staged)
            launcher = staged / "usr/bin/super-desktop"
            self.assertEqual(launcher.resolve(), staged / "usr/lib/super-desktop/super-desktop-client")
            self.assertTrue(launcher.resolve().with_name("super-desktop").is_file())
            self.assertTrue(os.access(launcher, os.X_OK))
            self.assertTrue((staged / "usr/share/super-desktop/assets/icons/hicolor/test.svg").is_file())
            self.assertFalse((staged / "home").exists())

    def test_recipe_pins_release_and_checksum(self):
        text = PACKAGE["recipe"]("1.2.3", "MIT", "a" * 64, ["gtk4>=4.18", "tmux"])
        self.assertIn("pkgver=1.2.3", text)
        self.assertIn("releases/download/v$pkgver/", text)
        self.assertIn("a" * 64, text)
        self.assertNotIn("SKIP", text)
        subprocess.run(["bash", "-n"], input=text, text=True, check=True)

    def test_omarchy_upstream_metadata_matches_makepkg_source_and_checksum_arrays(self):
        metadata = json.loads((ROOT / "packaging/omarchy/package.json").read_text())
        upstream = metadata["upstream"]
        version, checksum = "1.2.3", "a" * 64
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "PKGBUILD"
            path.write_text(PACKAGE["recipe"](version, "GPL-3.0-only", checksum, ["gtk4>=4.18"], omarchy=True))
            values = subprocess.check_output([
                "bash", "-c", 'source "$1"; printf "%s\\n" "${source_x86_64[@]}" "${sha256sums_x86_64[@]}" "${depends[@]}"',
                "bash", str(path),
            ], text=True).splitlines()
        asset = upstream["assets"]["x86_64"].format(pkgver=version)
        self.assertEqual(values[0], f"{asset}::https://github.com/{upstream['github']}/releases/download/v{version}/{asset}")
        self.assertEqual(values[1:], [checksum, "gtk4>=4.18"])
        self.assertEqual(upstream["checksums"], "SHA256SUMS")
        self.assertEqual(metadata["source"], "local")

    def test_dependency_notices_preserve_nested_notices_and_reject_unreviewed_terms(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fallback = root / "packaging/arch/licenses"
            fallback.mkdir(parents=True)
            (fallback / "sources.json").write_text("{}")
            crate = root / "crate"
            (crate / "third_party").mkdir(parents=True)
            (crate / "LICENSE").write_text("Upstream copyright and license")
            (crate / "third_party/NOTICE").write_text("Additional attribution")
            package = {"id": "fixture", "name": "fixture", "version": "1.0.0", "license": "MIT",
                       "source": "registry", "manifest_path": str(crate / "Cargo.toml")}
            metadata = {"packages": [package], "resolve": {"nodes": [{"id": "fixture"}]}}
            text, inventory = SOURCES["dependency_notices"](root, metadata)
            self.assertIn("Upstream copyright and license", text)
            self.assertIn("Additional attribution", text)
            self.assertEqual(len(inventory), 1)
            package["license"] = "LicenseRef-Unreviewed"
            with self.assertRaisesRegex(ValueError, "Unreviewed license"):
                SOURCES["dependency_notices"](root, metadata)

    def test_missing_crate_license_requires_exact_version_and_verified_fallback(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fallback = root / "packaging/arch/licenses"
            fallback.mkdir(parents=True)
            (fallback / "sources.json").write_text("{}")
            crate = root / "crate"
            crate.mkdir()
            package = {"id": "fixture", "name": "fixture", "version": "1.0.0", "license": "MIT",
                       "source": "registry", "manifest_path": str(crate / "Cargo.toml")}
            metadata = {"packages": [package], "resolve": {"nodes": [{"id": "fixture"}]}}
            with self.assertRaisesRegex(ValueError, "Missing license text"):
                SOURCES["dependency_notices"](root, metadata)
            data = b"Original upstream license"
            (fallback / "license.txt").write_bytes(data)
            (fallback / "sources.json").write_text(json.dumps({"fixture@1.0.0": {
                "file": "license.txt", "url": "https://example.org/pinned/LICENSE",
                "sha256": hashlib.sha256(data).hexdigest(),
            }}))
            self.assertIn(data.decode(), SOURCES["dependency_notices"](root, metadata)[0])
            (fallback / "license.txt").write_text("changed")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                SOURCES["dependency_notices"](root, metadata)
            package["version"] = "2.0.0"
            with self.assertRaisesRegex(ValueError, "Missing license text"):
                SOURCES["dependency_notices"](root, metadata)


if __name__ == "__main__":
    unittest.main()
