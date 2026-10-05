"""Prepare neutral identifiers for the Arch package's exported source tree."""
import hashlib
import json
from pathlib import Path

# Website-sourced or restricted artwork is replaced in the exported build only.
INITIALS = {
    "claude": "CL", "pi": "PI", "hermes": "HE", "crush": "CR",
    "kiro": "KI", "cursor": "CU", "kimi": "KM", "codex": "CX", "grok": "GR",
}
# Original geometric lettering; no font files or vendor paths are embedded.
LETTERS = {
    "C": "111/100/100/100/111", "E": "111/100/110/100/111",
    "G": "111/100/101/101/111", "H": "101/101/111/101/101",
    "I": "111/010/010/010/111", "K": "101/101/110/101/101",
    "L": "100/100/100/100/111", "M": "101/111/111/101/101",
    "P": "110/101/110/100/100", "R": "110/101/110/101/101",
    "U": "101/101/101/101/111", "X": "101/101/010/101/101",
}


def initials_svg(initials, color):
    cells = []
    for index, letter in enumerate(initials):
        for y, row in enumerate(LETTERS[letter].split("/")):
            for x, cell in enumerate(row):
                if cell == "1":
                    cells.append(f'<rect x="{1 + index * 4 + x}" y="{2 + y}" width="1" height="1"/>')
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 9 9">'
            '<defs><mask id="letters" maskUnits="userSpaceOnUse" x="0" y="0" width="9" height="9">'
            '<g fill="white">' + ''.join(cells) + '</g></mask></defs>'
            f'<rect width="9" height="9" fill="{color}" mask="url(#letters)"/></svg>\n').encode()


def prepare_assets(source):
    """Change only a disposable source export, never the developer checkout."""
    source = Path(source)
    if (source / ".git").exists():
        raise ValueError("Package artwork must be prepared in an exported source tree")
    directory = source / "assets/logos"
    manifest_path = directory / "harness-logos.json"
    manifest = json.loads(manifest_path.read_text())
    for key, initials in INITIALS.items():
        entry = manifest[key]
        files = {}
        for mode, color in (("light", "#111111"), ("dark", "#ffffff"), ("themed", "#123456")):
            name = entry[mode]
            # Some original marks share their light/dark filename. Their
            # neutral fallback uses the themed accent; normal rendering uses
            # the dedicated themed variant.
            if entry["light"] == entry["dark"] and mode != "themed":
                color = "#808080"
            data = initials_svg(initials, color)
            (directory / name).write_bytes(data)
            files[name] = hashlib.sha256(data).hexdigest()
        manifest[key] = {
            **{mode: entry[mode] for mode in ("light", "dark", "themed")},
            "source": "SUPER DESKTOP original geometric initials",
            "license": "GPL-3.0-only", "files": files,
        }
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    keep = {name for entry in manifest.values() for name in entry["files"]}
    keep.update(("harness-logos.json", "ATTRIBUTION.md", "LICENSES.md"))
    for path in directory.iterdir():
        if path.is_file() and path.name not in keep:
            path.unlink()
    attribution = directory / "ATTRIBUTION.md"
    attribution.write_text(
        "# Packaged harness identifiers\n\n"
        "The following harnesses use original SUPER DESKTOP geometric initials,\n"
        "licensed GPL-3.0-only: " + ", ".join(INITIALS) + ".\n\n"
        "Other marks retain their upstream licenses and attribution below.\n"
        "Product names identify integrations; no endorsement is implied.\n\n"
        + "\n".join(f"- {key}: {entry['source']}" for key, entry in manifest.items() if key not in INITIALS)
        + "\n\nSee LICENSES.md for upstream notices.\n"
    )
