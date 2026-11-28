#!/usr/bin/env bash
# Generate the dedicated Ed25519 Pacman repository signing key once, store its
# public key and fingerprint in the repository, save an offline backup outside
# git, and upload the private key to GitHub Actions secrets.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUB_KEY="$ROOT/packaging/arch/super-desktop.gpg"
FPR_FILE="$ROOT/packaging/arch/super-desktop.gpg.fingerprint"
INSTALL_SH="$ROOT/install.sh"
BACKUP_DIR="${SUPER_DESKTOP_KEY_BACKUP_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/super-desktop-signing}"
BACKUP_KEY="$BACKUP_DIR/private-key.asc"
BACKUP_PUB="$BACKUP_DIR/public-key.asc"
BACKUP_FPR="$BACKUP_DIR/fingerprint.txt"

FORCE=0
case "${1:-}" in
--force) FORCE=1 ;;
"") ;;
-h | --help)
    echo "Usage: $0 [--force]"
    exit 0
    ;;
*)
    echo "Error: Unknown argument: $1" >&2
    exit 1
    ;;
esac

command -v gpg >/dev/null 2>&1 || {
    echo "Error: gpg is required" >&2
    exit 1
}
command -v gh >/dev/null 2>&1 || {
    echo "Error: gh CLI is required" >&2
    exit 1
}
gh auth status >/dev/null 2>&1 || {
    echo "Error: gh is not authenticated. Run 'gh auth login' first." >&2
    exit 1
}

if ((FORCE == 0)) && [[ -s "$PUB_KEY" || -s "$FPR_FILE" ]]; then
    echo "Error: Signing key files already exist ($PUB_KEY). Pass --force to replace them." >&2
    exit 1
fi

GNUPG_WORK="$(mktemp -d)"
chmod 700 "$GNUPG_WORK"
trap 'rm -rf "$GNUPG_WORK"' EXIT
export GNUPGHOME="$GNUPG_WORK"

gpg --batch --gen-key <<'EOF'
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign
Name-Real: SUPER DESKTOP Package Signing
Name-Email: packages@superdesktop.dmytrogladkyi.com
Expire-Date: 5y
%no-protection
%commit
EOF

FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
[[ "$FPR" =~ ^[0-9A-F]{40}$ ]] || {
    echo "Error: Could not read generated 40-character fingerprint" >&2
    exit 1
}

mkdir -p "$(dirname "$PUB_KEY")"
gpg --armor --export "$FPR" >"$PUB_KEY"
printf '%s\n' "$FPR" >"$FPR_FILE"

if ! grep -q '^REPO_KEY_FINGERPRINT=' "$INSTALL_SH"; then
    echo "Error: REPO_KEY_FINGERPRINT= line not found in $INSTALL_SH" >&2
    exit 1
fi
python3 - "$INSTALL_SH" "$ROOT/docs/install/pacman-repo.sh" "$ROOT/docs/install/direct-pkg.sh" "$FPR" <<'PY'
import re, sys
from pathlib import Path
install_sh, pacman_repo_sh, direct_pkg_sh, fpr = (Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4])
for path, var in (
    (install_sh, "REPO_KEY_FINGERPRINT"),
    (pacman_repo_sh, "REPO_FPR"),
    (direct_pkg_sh, "REPO_FPR"),
):
    text = path.read_text()
    updated, count = re.subn(rf'^{var}=".*"$', f'{var}="{fpr}"', text, count=1, flags=re.M)
    if count != 1:
        raise SystemExit(f"Failed to update {var} in {path}")
    path.write_text(updated)
PY

mkdir -p "$BACKUP_DIR"
chmod 700 "$BACKUP_DIR"
umask 077
gpg --armor --export-secret-keys "$FPR" >"$BACKUP_KEY"
chmod 600 "$BACKUP_KEY"
cp -f "$PUB_KEY" "$BACKUP_PUB"
chmod 644 "$BACKUP_PUB"
cp -f "$FPR_FILE" "$BACKUP_FPR"
chmod 644 "$BACKUP_FPR"

gpg --armor --export-secret-keys "$FPR" | gh secret set PACMAN_GPG_PRIVATE_KEY

echo "✓ Generated Ed25519 signing key: $FPR"
echo "✓ Public key written to: $PUB_KEY"
echo "✓ Fingerprint written to: $FPR_FILE"
echo "✓ Updated REPO_KEY_FINGERPRINT in: $INSTALL_SH"
echo "✓ Private/public key backup saved outside git in: $BACKUP_DIR"
echo "✓ Uploaded PACMAN_GPG_PRIVATE_KEY secret to GitHub Actions"
echo ""
echo "Remaining steps:"
echo "  1. Commit and push packaging/arch/super-desktop.gpg, packaging/arch/super-desktop.gpg.fingerprint, and install.sh"
echo "  2. Switch GitHub Pages to GitHub Actions:"
echo "     gh api -X PUT repos/gladimdim/super-desktop/pages -f build_type=workflow"
echo "  3. Cut the next release when ready; CI will build x86_64 + aarch64, sign the repo, and deploy Pages."
