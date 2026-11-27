#!/usr/bin/env bash
# Exercise scripts/build-pacman-repo.sh with an ephemeral GPG key and dummy Arch package.
set -euo pipefail

cd "$(dirname "$0")/.."

for cmd in gpg repo-add makepkg bsdtar; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        echo "Skipping pacman_repo_smoke.sh: $cmd is not installed."
        exit 0
    fi
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

export GNUPGHOME="$work/gnupg"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"

gpg --batch --gen-key <<'EOF'
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign
Name-Real: SUPER DESKTOP Test Signing
Name-Email: test@example.com
Expire-Date: 1y
%no-protection
%commit
EOF

fpr="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
[[ "$fpr" =~ ^[0-9A-F]{40}$ ]]

pkgbuild_dir="$work/pkgbuild"
mkdir -p "$pkgbuild_dir"
cat >"$pkgbuild_dir/PKGBUILD" <<'EOF'
pkgname=super-desktop-bin
pkgver=2.1.2
pkgrel=1
pkgdesc='Dummy test package for repo smoke check'
arch=('x86_64')
license=('GPL-3.0-only')
package() {
    install -dm755 "$pkgdir/usr/bin"
    printf '#!/bin/sh\necho 2.1.2\n' >"$pkgdir/usr/bin/super-desktop"
    chmod 755 "$pkgdir/usr/bin/super-desktop"
}
EOF

(cd "$pkgbuild_dir" && PKGEXT='.pkg.tar.zst' makepkg --nodeps --nocheck >/dev/null 2>&1)
pkg="$(ls "$pkgbuild_dir"/super-desktop-bin-2.1.2-1-*.pkg.tar.zst)"

repo_dir="$work/repo/x86_64"
bash scripts/build-pacman-repo.sh \
    --repo-name super-desktop \
    --key-fingerprint "$fpr" \
    --output "$repo_dir" \
    "$pkg"

if [[ -n "$(find "$repo_dir" -type l)" ]]; then
    echo "Error: symlinks remained in $repo_dir" >&2
    exit 1
fi

base_pkg="$(basename "$pkg")"
for expected in \
    "super-desktop.db" \
    "super-desktop.db.sig" \
    "super-desktop.files" \
    "super-desktop.files.sig" \
    "$base_pkg" \
    "$base_pkg.sig"; do
    [[ -f "$repo_dir/$expected" ]] || {
        echo "Error: missing $expected in $repo_dir" >&2
        exit 1
    }
done

for sig in "$repo_dir"/*.sig; do
    gpg --batch --verify "$sig" "${sig%.sig}" >/dev/null 2>&1
done

bsdtar -tf "$repo_dir/super-desktop.db" | grep -q '^super-desktop-bin-2.1.2-1/'
echo "Pacman repository smoke checks passed."
