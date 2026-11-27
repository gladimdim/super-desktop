#!/usr/bin/env bash
# Build a GPG-signed static Pacman repository directory (zero symlinks).
#
# Usage:
#   scripts/build-pacman-repo.sh --repo-name <name> --key-fingerprint <40-hex> --output <dir> <pkg1.pkg.tar.zst> [...]
set -euo pipefail

REPO_NAME=""
FPR=""
OUTPUT=""
PACKAGES=()

while (($# > 0)); do
    case "$1" in
    --repo-name)
        REPO_NAME="${2:?Missing value for --repo-name}"
        shift 2
        ;;
    --key-fingerprint)
        FPR="${2:?Missing value for --key-fingerprint}"
        shift 2
        ;;
    --output)
        OUTPUT="${2:?Missing value for --output}"
        shift 2
        ;;
    -h | --help)
        sed -n '2,5p' "$0"
        exit 0
        ;;
    --)
        shift
        PACKAGES+=("$@")
        break
        ;;
    -*)
        echo "Error: Unknown option: $1" >&2
        exit 1
        ;;
    *)
        PACKAGES+=("$1")
        shift
        ;;
    esac
done

[[ -n "$REPO_NAME" && "$REPO_NAME" =~ ^[A-Za-z0-9._-]+$ ]] || {
    echo "Error: --repo-name must be a valid repository name" >&2
    exit 1
}
[[ "$FPR" =~ ^[0-9A-F]{40}$ ]] || {
    echo "Error: --key-fingerprint must be a 40-character uppercase hex fingerprint" >&2
    exit 1
}
[[ -n "$OUTPUT" ]] || {
    echo "Error: --output is required" >&2
    exit 1
}
((${#PACKAGES[@]} > 0)) || {
    echo "Error: Pass at least one .pkg.tar.zst package" >&2
    exit 1
}

for pkg in "${PACKAGES[@]}"; do
    [[ -r "$pkg" && "$pkg" == *.pkg.tar.zst ]] || {
        echo "Error: Package is missing or not a .pkg.tar.zst: $pkg" >&2
        exit 1
    }
done

mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"

for pkg in "${PACKAGES[@]}"; do
    base="$(basename "$pkg")"
    dst="$OUTPUT/$base"
    cp -f "$pkg" "$dst"
    gpg --batch --yes --no-armor --local-user "$FPR" --detach-sign --output "$dst.sig" "$dst"
done

repo-add --sign --key "$FPR" "$OUTPUT/$REPO_NAME.db.tar.gz" "$OUTPUT"/*.pkg.tar.zst

# Static hosting (GitHub Pages) does not preserve symlinks reliably: replace
# repo-add's .db / .files symlinks with regular dereferenced files.
for ext in db db.sig files files.sig; do
    link="$OUTPUT/$REPO_NAME.$ext"
    if [[ -L "$link" ]]; then
        tmp="$(mktemp "$OUTPUT/.${REPO_NAME}.${ext}.XXXXXX")"
        cp -L "$link" "$tmp"
        mv -f "$tmp" "$link"
    fi
done

rm -f "$OUTPUT/$REPO_NAME".*.old "$OUTPUT/$REPO_NAME".*.old.sig

if [[ -n "$(find "$OUTPUT" -type l)" ]]; then
    echo "Error: Symlinks remain in $OUTPUT" >&2
    exit 1
fi

for sig in "$OUTPUT"/*.sig; do
    [[ -f "$sig" ]] || continue
    gpg --batch --verify "$sig" "${sig%.sig}" >/dev/null 2>&1 || {
        echo "Error: Signature verification failed for $sig" >&2
        exit 1
    }
done

echo "✓ Built signed Pacman repository in $OUTPUT ($REPO_NAME)"
