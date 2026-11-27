#!/usr/bin/env bash
# Exercise install.sh's binary Pacman repository, GPG verification,
# partial-upgrade guard, and pinned release asset handling with mocks.
set -euo pipefail

cd "$(dirname "$0")/.."

for cmd in gpg sha256sum awk; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "Error: $cmd is required" >&2
        exit 1
    }
done

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Source install.sh functions without running main.
sed '$d' install.sh >"$work/install_lib.sh"

# Generate an ephemeral GPG key for real gpg --show-keys verification.
export GNUPGHOME="$work/author-gnupg"
mkdir -p "$GNUPGHOME"
chmod 700 "$GNUPGHOME"
gpg --batch --gen-key <<'EOF'
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign
Name-Real: SUPER DESKTOP Test Repo
Name-Email: test@example.com
Expire-Date: 1y
%no-protection
%commit
EOF
REAL_FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
[[ "$REAL_FPR" =~ ^[0-9A-F]{40}$ ]]
gpg --armor --export "$REAL_FPR" >"$work/super-desktop.gpg"

# Dummy package archive + SHA256SUMS for pinned release tests.
printf 'dummy-pkg-contents\n' >"$work/super-desktop-bin-2.1.2-1-x86_64.pkg.tar.zst"
GOOD_SHA="$(sha256sum "$work/super-desktop-bin-2.1.2-1-x86_64.pkg.tar.zst" | awk '{ print $1 }')"
printf '%s  %s\n' "$GOOD_SHA" "super-desktop-bin-2.1.2-1-x86_64.pkg.tar.zst" >"$work/SHA256SUMS"
printf 'tampered-bytes\n' >"$work/tampered.pkg.tar.zst"

export WORK_DIR="$work"
export REAL_FPR

run_case() {
    local name="$1"
    shift
    (
        set -euo pipefail
        export CASE_NAME="$name"
        export HOME="$WORK_DIR/home-$name"
        mkdir -p "$HOME"
        PACMAN_CONF="$WORK_DIR/pacman-$name.conf"
        PACMAN_REPO_CONF="$WORK_DIR/pacman.d-$name/super-desktop.conf"
        printf '[omarchy]\nServer = https://pkgs.omarchy.org/edge/$arch\n' >"$PACMAN_CONF"

        export SUPER_DESKTOP_PACMAN_CONF="$PACMAN_CONF"
        export SUPER_DESKTOP_PACMAN_REPO_CONF="$PACMAN_REPO_CONF"
        export SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo"

        source "$WORK_DIR/install_lib.sh"

        # Mocks
        sudo() {
            printf 'sudo %s\n' "$*" >>"$WORK_DIR/calls-$CASE_NAME.log"
            case "$1" in
            mkdir) mkdir -p "${@:3}" ;;
            install)
                local mode="$3" src="$4" dst="$5"
                mkdir -p "$(dirname "$dst")"
                cp "$src" "$dst"
                chmod "$mode" "$dst"
                ;;
            *) "$@" ;;
            esac
        }
        pacman-key() {
            printf 'pacman-key %s\n' "$*" >>"$WORK_DIR/calls-$CASE_NAME.log"
            case "$1" in
            --list-keys) return 1 ;;
            --add | --lsign-key) return 0 ;;
            esac
        }
        omarchy() {
            printf 'omarchy %s\n' "$*" >>"$WORK_DIR/calls-$CASE_NAME.log"
            touch "$WORK_DIR/updated-$CASE_NAME"
            return 0
        }
        export -f sudo pacman-key omarchy

        "$@"
    )
}

# 1. Empty fingerprint makes pacman_repo_available return false safely.
case_empty_fpr() {
    REPO_KEY_FINGERPRINT=""
    curl() { return 0; }
    ! pacman_repo_available x86_64
}
run_case empty_fpr case_empty_fpr

# 2. Mismatched GPG key fingerprint is rejected loudly.
case_bad_fpr() {
    REPO_KEY_FINGERPRINT="0000000000000000000000000000000000000000"
    curl() {
        local out=""
        while (($# > 0)); do
            if [[ "$1" == "-o" ]]; then out="$2"; shift 2; else shift; fi
        done
        cp "$WORK_DIR/super-desktop.gpg" "$out"
    }
    if (import_repo_key) 2>"$WORK_DIR/err-bad-fpr"; then
        echo "Expected import_repo_key to fail on mismatched fingerprint" >&2
        exit 1
    fi
    grep -q "does not match expected fingerprint" "$WORK_DIR/err-bad-fpr"
}
run_case bad_fpr case_bad_fpr

# 3. Valid key + repo config + Omarchy pre-refresh-pacman hook (idempotent + working hook).
case_good_repo_config() {
    REPO_KEY_FINGERPRINT="$REAL_FPR"
    curl() {
        local out=""
        while (($# > 0)); do
            if [[ "$1" == "-o" ]]; then out="$2"; shift 2; else shift; fi
        done
        cp "$WORK_DIR/super-desktop.gpg" "$out"
    }
    import_repo_key
    configure_pacman_repo
    configure_pacman_repo
    [[ -f "$PACMAN_REPO_CONF" ]]
    grep -qF "Server = https://example.invalid/repo/\$arch" "$PACMAN_REPO_CONF"
    grep -qF "SigLevel = Required DatabaseRequired" "$PACMAN_REPO_CONF"
    [[ "$(grep -cFx "Include = $PACMAN_REPO_CONF" "$PACMAN_CONF")" -eq 1 ]]
    # Verify hook re-inserts Include when pacman.conf is overwritten by omarchy refresh pacman.
    hook="$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"
    [[ -x "$hook" ]]
    printf '[omarchy]\nServer = https://pkgs.omarchy.org/stable/$arch\n' >"$PACMAN_CONF"
    # Provide mock sudo on PATH for hook invocation.
    mkdir -p "$WORK_DIR/bin"
    printf '#!/bin/sh\nexec "$@"\n' >"$WORK_DIR/bin/sudo"
    chmod +x "$WORK_DIR/bin/sudo"
    PATH="$WORK_DIR/bin:$PATH" "$hook"
    grep -qxF "Include = $PACMAN_REPO_CONF" "$PACMAN_CONF"
}
run_case good_repo case_good_repo_config

# 4. Partial-upgrade guard triggers omarchy update when another installed package would upgrade.
case_partial_upgrade_guard() {
    pacman() {
        printf 'pacman %s\n' "$*" >>"$WORK_DIR/calls-partial.log"
        case "$1" in
        -Sy | -S) return 0 ;;
        -Sp)
            if [[ "$*" == *"--print-format %n"* ]]; then
                if [[ -f "$WORK_DIR/updated-partial" ]]; then
                    printf 'super-desktop-bin\n'
                else
                    printf 'gtk4\nsuper-desktop-bin\n'
                fi
            fi
            return 0
            ;;
        -Q)
            [[ "$2" == "gtk4" ]]
            ;;
        esac
    }
    SHELL=/bin/bash
    if (: </dev/tty) 2>/dev/null; then
        resolve_and_install_pacman_target -S super-desktop-bin </dev/null
    else
        # Headless CI runners have no /dev/tty; allocate a PTY like install_packages.sh.
        export -f pacman
        export PACMAN_CONF PACMAN_REPO_CONF WORK_DIR
        script -qefc 'bash -euo pipefail -c '\''source "$WORK_DIR/install_lib.sh"; resolve_and_install_pacman_target -S super-desktop-bin </dev/null'\''' /dev/null >/dev/null
    fi
    grep -q '^omarchy update$' "$WORK_DIR/calls-partial.log"
    grep -q '^sudo pacman -S --needed --noconfirm super-desktop-bin$' "$WORK_DIR/calls-partial.log"
}
run_case partial case_partial_upgrade_guard

# 5. Pinned release checks SHA256SUMS, aborts on mismatch, and returns 1 on 404 for fallback.
case_pinned_install() {
    REPO_KEY_FINGERPRINT=""
    pacman() {
        printf 'pacman %s\n' "$*" >>"$WORK_DIR/calls-pinned.log"
        return 0
    }
    # 404 returns 1 cleanly for source fallback.
    curl() { return 22; }
    if install_pinned_package "v1.1.17" "x86_64"; then
        echo "Expected 404 pinned package to return 1" >&2
        exit 1
    fi

    # Checksum mismatch hard-fails (exit != 0 inside subshell).
    curl() {
        local out="" url=""
        while (($# > 0)); do
            case "$1" in
            -o) out="$2"; shift 2 ;;
            -*) shift ;;
            *) url="$1"; shift ;;
            esac
        done
        case "$url" in
        *.pkg.tar.zst) cp "$WORK_DIR/tampered.pkg.tar.zst" "$out" ;;
        *SHA256SUMS) cp "$WORK_DIR/SHA256SUMS" "$out" ;;
        *) return 22 ;;
        esac
    }
    if (install_pinned_package "v2.1.2" "x86_64") 2>"$WORK_DIR/err-tamper"; then
        echo "Expected tampered package to die" >&2
        exit 1
    fi
    grep -q "Checksum mismatch" "$WORK_DIR/err-tamper"

    # Valid checksum installs via pacman -U.
    curl() {
        local out="" url=""
        while (($# > 0)); do
            case "$1" in
            -o) out="$2"; shift 2 ;;
            -*) shift ;;
            *) url="$1"; shift ;;
            esac
        done
        case "$url" in
        *.pkg.tar.zst) cp "$WORK_DIR/super-desktop-bin-2.1.2-1-x86_64.pkg.tar.zst" "$out" ;;
        *SHA256SUMS) cp "$WORK_DIR/SHA256SUMS" "$out" ;;
        *) return 22 ;;
        esac
    }
    install_pinned_package "v2.1.2" "x86_64"
    grep -q '^sudo pacman -U --noconfirm ' "$WORK_DIR/calls-pinned.log"
}
run_case pinned case_pinned_install

echo "Installer binary and repo checks passed."
