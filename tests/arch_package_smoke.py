"""Exercise package transactions and setup in a private root, never the desktop.

Usage: python3 tests/arch_package_smoke.py PACKAGE UPGRADE_PACKAGE
Requires bubblewrap user namespaces, pacman and two locally built package revisions.
Dependencies are verified against the host; only package files and a minimal shell
are installed in the private root. No package database or home on the host changes.
"""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


def snapshot(home):
    return {str(p.relative_to(home)): (os.readlink(p) if p.is_symlink() else hashlib.sha256(p.read_bytes()).hexdigest())
            for p in home.rglob("*") if p.is_symlink() or p.is_file()}


def main():
    first, upgrade = (Path(p).resolve() for p in sys.argv[1:])
    if os.geteuid() == 0:
        raise SystemExit("Run as an ordinary user; root privileges are confined to a user namespace")
    with tempfile.TemporaryDirectory(prefix="sd-pacman-smoke-") as temp:
        work = Path(temp)
        root = work / "root"
        for directory in ("usr/bin", "usr/lib", "var/lib/pacman", "var/cache/pacman/pkg", "tmp"):
            (root / directory).mkdir(parents=True)
        for name, target in (("bin", "usr/bin"), ("lib", "usr/lib"), ("lib64", "usr/lib")):
            (root / name).symlink_to(target)
        # Pacman's actual install/upgrade notices run in chroot with this shell.
        shutil.copyfile("/usr/bin/bash", root / "usr/bin/bash")
        (root / "usr/bin/bash").chmod(0o755)
        (root / "usr/bin/sh").symlink_to("bash")
        for line in run("ldd", "/usr/bin/bash").splitlines():
            tokens = line.strip().split()
            path = tokens[2] if len(tokens) > 2 and tokens[1] == "=>" else tokens[0] if tokens else ""
            if path.startswith("/"):
                shutil.copy2(path, root / "usr/lib" / Path(path).name)
        config = work / "pacman.conf"
        config.write_text("[options]\nArchitecture = auto\nSigLevel = Never\n")
        dependencies = []
        for line in run("bsdtar", "-xOf", str(first), ".PKGINFO").splitlines():
            if line.startswith("depend = "):
                dependencies.append(line.removeprefix("depend = "))
        run("pacman", "-T", *dependencies)

        def pacman(*args):
            return run("bwrap", "--unshare-user", "--uid", "0", "--gid", "0", "--cap-add", "CAP_SYS_CHROOT", "--unshare-net",
                       "--ro-bind", "/", "/", "--bind", str(work), str(work), "--proc", "/proc", "--dev", "/dev",
                       "pacman", "--root", str(root), "--dbpath", str(root / "var/lib/pacman"),
                       "--logfile", str(work / "pacman.log"), "--config", str(config), "--noconfirm", *args)

        output = pacman("-Udd", str(first))
        assert "Run super-desktop-setup" in output, output
        run(str(root / "usr/bin/super-desktop"), "--version")
        run(str(root / "usr/lib/super-desktop/super-desktop"), "--version")
        notices = root / "usr/share/licenses/super-desktop-bin"
        for name in ("LICENSE", "THIRD_PARTY_LICENSES.txt", "RUST_LIBRARY_COPYRIGHT.html", "SOURCE"):
            assert (notices / name).stat().st_size > 0
        env = dict(os.environ)
        for key in ("HYPRLAND_INSTANCE_SIGNATURE", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY", "TMUX", "TMUX_PANE"):
            env.pop(key, None)
        homes = []
        for migrating in (False, True):
            home = work / ("migration-home" if migrating else "fresh-home")
            hypr = home / ".config/hypr"
            hypr.mkdir(parents=True)
            (hypr / "hyprland.lua").write_text("-- isolated Omarchy Lua configuration\n")
            state = home / ".config/super-desktop/state.json"
            state.parent.mkdir()
            state.write_text('{"notes":["preserve me"]}\n')
            if migrating:
                launcher = home / ".local/bin/super-desktop"
                launcher.parent.mkdir(parents=True)
                launcher.symlink_to("/unmodified-checkout/target/release/super-desktop-client")
                desktop = home / ".local/share/applications/super-desktop.desktop"
                desktop.parent.mkdir(parents=True)
                desktop.write_text("[Desktop Entry]\nExec=/old/super-desktop toggle\n")
                (hypr / "bindings.lua").write_text('o.bind("SUPER + A", "Existing", "super-desktop toggle")\n')
            args = ("--migrate",) if migrating else ()
            run(str(root / "usr/bin/super-desktop-setup"), *args, env={**env, "HOME": str(home)})
            before = snapshot(home)
            run(str(root / "usr/bin/super-desktop-setup"), *args, env={**env, "HOME": str(home)})
            assert snapshot(home) == before, "Setup is not idempotent"
            assert state.read_text() == '{"notes":["preserve me"]}\n'
            if migrating:
                assert os.readlink(launcher) == "/usr/bin/super-desktop"
                assert launcher.with_name("super-desktop.before-package").is_symlink()
                assert '"SUPER + A"' in (hypr / "bindings.lua").read_text()
            homes.append((home, before))
        output = pacman("-Udd", str(upgrade))
        assert "Restart SUPER DESKTOP" in output, output
        expected_version = next(line.removeprefix("pkgver = ") for line in
                                run("bsdtar", "-xOf", str(upgrade), ".PKGINFO").splitlines()
                                if line.startswith("pkgver = "))
        assert pacman("-Q", "super-desktop-bin").strip() == f"super-desktop-bin {expected_version}"
        pacman("-Qkk", "super-desktop-bin")
        output = pacman("-Udd", str(upgrade))
        assert "Restart SUPER DESKTOP" in output, output
        pacman("-Rdd", "super-desktop-bin")
        assert not (root / "usr/bin/super-desktop").exists()
        assert not (root / "usr/bin/super-desktop").is_symlink()
        assert not (root / "usr/bin/super-desktop-setup").exists()
        assert not (root / "usr/lib/super-desktop").exists()
        assert not (root / "usr/share/super-desktop").exists()
        assert not (root / "usr/share/applications/super-desktop.desktop").exists()
        assert not notices.exists()
        for home, before in homes:
            assert snapshot(home) == before, "Package transaction changed user data"
        print("PASS: host dependencies, fresh install, real scriptlets, both entry points, notices, setup, migration,")
        print("      idempotence, upgrade, reinstall, file integrity, removal and preservation of user data.")


if __name__ == "__main__":
    main()
