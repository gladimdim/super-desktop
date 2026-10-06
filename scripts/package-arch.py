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


def stage(root, binaries, destination, sources=None):
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
    if sources is not None:
        for name in ("THIRD_PARTY_LICENSES.txt", "RUST_LIBRARY_COPYRIGHT.html", "DEPENDENCIES.json"):
            install(sources / name, f"usr/share/licenses/super-desktop-bin/{name}")
        version, _ = release_metadata(root)
        source_url = f"https://github.com/gladimdim/super-desktop/releases/download/v{version}/super-desktop-{version}-source.tar.gz"
        notice = destination / "usr/share/licenses/super-desktop-bin/SOURCE"
        notice.write_text(f"Corresponding source, vendored Rust dependencies and build instructions:\n{source_url}\n")


def recipe(version, license_id, checksum, dependencies, *, omarchy=False):
    quote = shlex.quote
    deps = " ".join(quote(value) for value in dependencies)
    suffix = "_x86_64" if omarchy else ""
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
source{suffix}=("super-desktop-$pkgver-linux-x86_64.tar.gz::$url/releases/download/v$pkgver/super-desktop-$pkgver-linux-x86_64.tar.gz")
sha256sums{suffix}=('{checksum}')

package() {{
    cp -a "$srcdir/usr" "$pkgdir/"
}}
'''


def write_recipes(root, output, version, license_id, checksum, dependencies):
    for omarchy, directory in (
        (False, output / "aur"),
        (True, output / "omarchy/pkgbuilds/super-desktop-bin"),
    ):
        directory.mkdir(parents=True)
        (directory / "PKGBUILD").write_text(
            recipe(version, license_id, checksum, dependencies, omarchy=omarchy)
        )
        shutil.copyfile(root / "packaging/arch/super-desktop-bin.install", directory / "super-desktop-bin.install")
        srcinfo = subprocess.check_output(["makepkg", "--printsrcinfo"], cwd=directory, text=True)
        (directory / ".SRCINFO").write_text(srcinfo)
        if omarchy:
            (directory / ".omarchy").mkdir()
            shutil.copyfile(root / "packaging/omarchy/package.json", directory / ".omarchy/package.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "target/release")
    parser.add_argument("--source-dir", type=Path, required=True, help="Exported source used to build both binaries")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--release-tag", required=True)
    args = parser.parse_args()
    version, license_id = release_metadata(ROOT)
    check_release(ROOT, args.release_tag, version)
    if release_metadata(args.source_dir) != (version, license_id):
        parser.error("Exported source version/license must match the release checkout")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if f"Commit: {commit}\n" not in (args.source_dir / "BUILDING").read_text():
        parser.error("Exported source must come from this release commit")
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
    source_archive = output / f"super-desktop-{version}-source.tar.gz"
    if archive.exists() or source_archive.exists() or (output / "aur").exists() or (output / "omarchy").exists():
        parser.error("Output already contains a package; use a fresh output directory")
    with tempfile.TemporaryDirectory() as temp:
        sources = args.source_dir.resolve()
        if (sources / "target").exists() or (sources / ".git").exists():
            parser.error("Keep build output and git metadata outside the exported source")
        with tarfile.open(source_archive, "w:gz") as tar:
            tar.add(sources, arcname=f"super-desktop-{version}-source", filter=normalize_owner)
        staged = Path(temp) / "staged"
        stage(sources, args.binary_dir, staged, sources)
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(staged / "usr", arcname="usr", filter=normalize_owner)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    source_checksum = hashlib.sha256(source_archive.read_bytes()).hexdigest()
    (output / "SHA256SUMS").write_text(f"{checksum}  {archive.name}\n{source_checksum}  {source_archive.name}\n")
    write_recipes(ROOT, output, version, license_id, checksum, dependencies)
    print(f"Prepared {archive}, {output / 'aur'} and {output / 'omarchy'}; nothing uploaded.")


def normalize_owner(info):
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    return info


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error))
