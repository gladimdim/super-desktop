#!/usr/bin/env python3
"""Stage release binaries and generate the checksum-pinned AUR submission."""
import argparse
import hashlib
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
LIBRARIES = ("glibc", "gcc-libs", "glib2", "gtk4", "gtk4-layer-shell", "vte4", "cairo", "pango", "gdk-pixbuf2", "graphene")
RUNTIME = ("bash", "coreutils", "python", "tmux", "wl-clipboard", "libnotify", "sqlite", "avahi", "bubblewrap", "poppler")


def release_metadata(root):
    package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
    version = package["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("An X.Y.Z release version is required")
    license_id = package.get("license", "")
    if not license_id or not (root / "LICENSE").is_file():
        raise ValueError("Before distributing: choose the project's license, set package.license in Cargo.toml, and add its text as LICENSE. Asset licenses do not license the application.")
    if not re.fullmatch(r"[A-Za-z0-9.()+ -]+", license_id):
        raise ValueError("package.license must be an SPDX license expression")
    return version, license_id


def check_release(root, tag, version):
    def git(*args):
        return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
    if tag != f"v{version}":
        raise ValueError("Release tag must match Cargo.toml")
    if git("cat-file", "-t", f"refs/tags/{tag}") != "tag":
        raise ValueError("Release tag must be annotated")
    if git("rev-parse", f"refs/tags/{tag}^{{commit}}") != git("rev-parse", "HEAD"):
        raise ValueError("Check out the release tag before packaging")
    if git("status", "--porcelain", "--untracked-files=normal"):
        raise ValueError("Release packaging requires a clean checkout")


def stage(root, binaries, destination):
    def install(source, relative, mode=0o644):
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(mode)
    for name in ("super-desktop", "super-desktop-client"):
        install(binaries / name, f"usr/lib/super-desktop/{name}", 0o755)
    install(root / "packaging/arch/super-desktop-setup", "usr/bin/super-desktop-setup", 0o755)
    (destination / "usr/bin/super-desktop").symlink_to("../lib/super-desktop/super-desktop-client")
    install(root / "super-desktop.desktop", "usr/share/applications/super-desktop.desktop")
    install(root / "LICENSE", "usr/share/licenses/super-desktop-bin/LICENSE")
    for folder in ("logos", "icons"):
        shutil.copytree(root / "assets" / folder, destination / "usr/share/super-desktop/assets" / folder)
    install(root / "assets/logos/LICENSES.md", "usr/share/licenses/super-desktop-bin/ASSET-LICENSES")
    install(root / "assets/logos/ATTRIBUTION.md", "usr/share/licenses/super-desktop-bin/ASSET-ATTRIBUTION")


def recipe(version, license_id, checksum, dependencies):
    quote = shlex.quote
    deps = " ".join(quote(value) for value in dependencies)
    return f'''# Maintained from the SUPER DESKTOP release artifacts.
pkgname=super-desktop-bin
pkgver={version}
pkgrel=1
pkgdesc='Sticky notes and AI terminal overlay for Omarchy / Hyprland'
arch=('x86_64')
url='https://github.com/gladimdim/super-desktop'
license=({quote(license_id)})
depends=({deps})
optdepends=('hyprland: Wayland compositor for the overlay')
provides=("super-desktop=$pkgver")
conflicts=('super-desktop' 'super-desktop-git')
options=('!strip' '!debug')
install=super-desktop-bin.install
source=("super-desktop-$pkgver-linux-x86_64.tar.gz::$url/releases/download/v$pkgver/super-desktop-$pkgver-linux-x86_64.tar.gz")
sha256sums=('{checksum}')

package() {{
    cp -a "$srcdir/usr" "$pkgdir/"
}}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "target/release")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--release-tag", required=True)
    args = parser.parse_args()
    version, license_id = release_metadata(ROOT)
    check_release(ROOT, args.release_tag, version)
    if subprocess.check_output(["uname", "-m"], text=True).strip() != "x86_64":
        parser.error("This package is built for x86_64 only")
    # Use the build machine's library versions as conservative ABI floors.
    dependencies = list(RUNTIME)
    for name in LIBRARIES:
        found, installed = subprocess.check_output(["pacman", "-Q", name], text=True).split()
        dependencies.append(f"{found}>={installed}")
    for name in ("super-desktop", "super-desktop-client"):
        actual = subprocess.check_output([str(args.binary_dir / name), "--version"], text=True).strip()
        if actual.split()[-1] != version:
            parser.error(f"{name} version does not match {version}: {actual}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"super-desktop-{version}-linux-x86_64.tar.gz"
    if archive.exists() or (output / "aur").exists():
        parser.error("Output already contains a package; use a fresh output directory")
    with tempfile.TemporaryDirectory() as temp:
        staged = Path(temp)
        stage(ROOT, args.binary_dir, staged)
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(staged / "usr", arcname="usr", filter=normalize_owner)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / "SHA256SUMS").write_text(f"{checksum}  {archive.name}\n")
    aur = output / "aur"
    aur.mkdir()
    (aur / "PKGBUILD").write_text(recipe(version, license_id, checksum, dependencies))
    shutil.copyfile(ROOT / "packaging/arch/super-desktop-bin.install", aur / "super-desktop-bin.install")
    srcinfo = subprocess.check_output(["makepkg", "--printsrcinfo"], cwd=aur, text=True)
    (aur / ".SRCINFO").write_text(srcinfo)
    print(f"Prepared {archive} and {aur}; nothing uploaded.")


def normalize_owner(info):
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    return info


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error))
