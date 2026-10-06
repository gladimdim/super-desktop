#!/usr/bin/env bash
# Usage: publish-aur.sh <generated aur directory>
# Uses the caller's SSH authentication; never creates or prints a private key.
set -euo pipefail
submission="$(realpath "${1:?Pass the generated aur directory}")"
for file in PKGBUILD .SRCINFO super-desktop-bin.install; do
    test -s "$submission/$file"
done
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Recompute metadata rather than trusting a stale .SRCINFO.
(cd "$submission" && makepkg --printsrcinfo) >"$work/srcinfo"
cmp "$submission/.SRCINFO" "$work/srcinfo"
version="$(sed -n 's/^\s*pkgver = //p' "$submission/.SRCINFO")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
source_url="https://github.com/gladimdim/super-desktop/releases/download/v$version/super-desktop-$version-linux-x86_64.tar.gz"
checksum="$(sed -n 's/^\s*sha256sums = //p' "$submission/.SRCINFO")"
[[ "$checksum" =~ ^[a-f0-9]{64}$ ]]
curl --proto '=https' --tlsv1.2 -fL --retry 3 "$source_url" -o "$work/release.tar.gz"
printf '%s  %s\n' "$checksum" "$work/release.tar.gz" | sha256sum --check --status
git clone ssh://aur@aur.archlinux.org/super-desktop-bin.git "$work/repo"
cd "$work/repo"
if [[ -f .SRCINFO ]]; then
    previous="$(sed -n 's/^\s*pkgver = //p' .SRCINFO)"
    if [[ "$(vercmp "$version" "$previous")" -le 0 ]]; then
        echo "Refusing to replace AUR $previous with $version. A newer release is required." >&2
        exit 1
    fi
fi
cp "$submission/PKGBUILD" "$submission/.SRCINFO" "$submission/super-desktop-bin.install" .
git add PKGBUILD .SRCINFO super-desktop-bin.install
git -c user.name="${AUR_COMMIT_NAME:?Set AUR_COMMIT_NAME}" \
    -c user.email="${AUR_COMMIT_EMAIL:?Set AUR_COMMIT_EMAIL}" \
    commit -m "Update super-desktop-bin to $version"
git push origin HEAD:master
