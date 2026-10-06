#!/usr/bin/env python3
"""Bundle the release's source and dependency notices for Arch distribution."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import runpy

PREPARE_ASSETS = runpy.run_path(str(Path(__file__).with_name("arch_assets.py")))["prepare_assets"]


# Reviewed Linux dependency declarations. New terms require review before shipping.
REVIEWED_LICENSES = {
    "MIT", "Apache-2.0", "ISC", "BSD-3-Clause", "Unicode-3.0",
    "MIT OR Apache-2.0", "Apache-2.0 OR MIT", "MIT/Apache-2.0",
    "Apache-2.0 OR ISC OR MIT", "Apache-2.0 AND ISC", "Unlicense OR MIT",
    "Apache-2.0 WITH LLVM-exception",
    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
    "(MIT OR Apache-2.0) AND Unicode-3.0",
}


def license_files(directory):
    return sorted(p for p in directory.rglob("*") if p.is_file() and any(
        token in p.name.lower() for token in ("license", "licence", "copying", "copyright", "notice")
    ))


def dependency_notices(root, metadata):
    active = {node["id"] for node in metadata["resolve"]["nodes"]}
    fallback_dir = root / "packaging/arch/licenses"
    fallbacks = json.loads((fallback_dir / "sources.json").read_text())
    sections = ["SUPER DESKTOP Linux Rust dependency notices\n"
                "Each dependency retains its own license. Alternative licenses remain alternatives.\n"]
    inventory = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] not in active or package["source"] is None:
            continue
        name, version, license_id = package["name"], package["version"], package["license"]
        if license_id not in REVIEWED_LICENSES:
            raise ValueError(f"Unreviewed license for {name} {version}: {license_id}")
        directory = Path(package["manifest_path"]).parent
        files = license_files(directory)
        sections.append(f"\n{'=' * 72}\n{name} {version}\nLicense: {license_id}\n"
                        f"Upstream: {package.get('repository') or ''}\n")
        if not files:
            fallback = fallbacks.get(f"{name}@{version}")
            if not fallback:
                raise ValueError(f"Missing license text for {name} {version}")
            path = fallback_dir / fallback["file"]
            data = path.read_bytes()
            if hashlib.sha256(data).hexdigest() != fallback["sha256"]:
                raise ValueError(f"License checksum mismatch: {path}")
            sections.append(f"Source: {fallback['url']}\n\n" + data.decode())
        for path in files:
            if not path.resolve().is_relative_to(directory.resolve()):
                raise ValueError(f"License path escapes crate: {path}")
            sections.append(f"\n--- {path.relative_to(directory)} ---\n" + path.read_text())
        inventory.append({"name": name, "version": version, "license": license_id})
    return "\n".join(sections), inventory


def prepare_sources(root, destination):
    """Use committed source and Cargo's checksum-verified, locked dependencies."""
    destination.mkdir(parents=True)
    with tempfile.TemporaryFile() as archive:
        # Website media is not a build input and contains screenshots of vendor
        # marks. Export build inputs and text documentation only.
        subprocess.run(["git", "archive", "HEAD", ".", ":(exclude)docs", ":(exclude)scripts/trailer"], cwd=root, stdout=archive, check=True)
        archive.seek(0)
        with tarfile.open(fileobj=archive) as tar:
            tar.extractall(destination, filter="data")
    PREPARE_ASSETS(destination)
    config = subprocess.check_output(
        ["cargo", "vendor", "--locked", "--versioned-dirs", str(destination / "vendor")],
        cwd=root, text=True,
    ).replace(str(destination / "vendor"), "vendor")
    config_dir = destination / ".cargo"
    config_dir.mkdir(exist_ok=True)
    if (config_dir / "config.toml").exists() or (config_dir / "config").exists():
        raise ValueError("Review existing Cargo configuration before adding vendored sources")
    (config_dir / "config.toml").write_text(config)
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--offline", "--format-version", "1",
        "--filter-platform", "x86_64-unknown-linux-gnu",
    ], cwd=destination, text=True))
    notices, inventory = dependency_notices(root, metadata)
    (destination / "THIRD_PARTY_LICENSES.txt").write_text(notices)
    (destination / "DEPENDENCIES.json").write_text(json.dumps(inventory, indent=2) + "\n")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    rustc = subprocess.check_output(["rustc", "--version"], text=True).strip()
    (destination / "BUILDING").write_text(
        f"SUPER DESKTOP corresponding source\nCommit: {commit}\nCompiler used: {rustc}\n\n"
        "On Omarchy / Arch x86_64, install build prerequisites:\n"
        "  sudo pacman -S --needed base-devel rust python gtk4 gtk4-layer-shell vte4\n"
        "Build without downloading Rust dependencies:\n"
        "  cargo build --release --locked --offline --bins\n"
        "Cargo dependencies are included in vendor/ with their original license terms.\n"
        "Package-specific neutral artwork is recorded in assets/logos/ATTRIBUTION.md.\n"
        "The system compiler and dynamically linked system libraries are installed separately.\n"
        "See README.md for runtime dependencies, configuration and installation.\n"
    )
    # Rust's standard library is linked into the binaries; preserve its notices too.
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    candidates = [sysroot / "share/doc/rust/COPYRIGHT-library.html"]
    if sysroot == Path("/usr"):
        candidates.append(Path("/usr/share/licenses/rust/COPYRIGHT-library.html.rustc"))
    copyright_file = next((p for p in candidates if p.is_file()), None)
    if copyright_file is None:
        raise ValueError("Rust standard-library notices missing from the compiler installation")
    shutil.copyfile(copyright_file, destination / "RUST_LIBRARY_COPYRIGHT.html")
    return inventory


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("Use a fresh source output directory")
    prepare_sources(Path(__file__).resolve().parents[1], args.output)
