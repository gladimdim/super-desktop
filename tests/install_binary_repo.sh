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

# Source install.sh and docs/install/pacman-repo.sh functions without running main.
sed '$d' install.sh >"$work/install_lib.sh"
sed '$d' docs/install/pacman-repo.sh >"$work/pacman_repo_lib.sh"

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
        printf '[options]\nArchitecture = auto\nDownloadUser = alpm\n\n[omarchy]\nServer = https://pkgs.omarchy.org/edge/$arch\n' >"$PACMAN_CONF"

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

# 4. Single-repo sync + partial-upgrade guard (never runs -Syu or omarchy update).
case_partial_upgrade_guard() {
    pacman() {
        printf 'pacman %s\n' "$*" >>"$WORK_DIR/calls-partial.log"
        if [[ "$1" == "--config" ]]; then
            local cfg="$2"
            cp "$cfg" "$WORK_DIR/synced-config-partial.conf"
            shift 2
        fi
        case "$1" in
        -Sy) return 0 ;;
        -S) return 0 ;;
        -Sp)
            if [[ "$*" == *"--print-format %n"* ]]; then
                if [[ -f "$WORK_DIR/clean-partial" ]]; then
                    printf 'vte4\nsuper-desktop-bin\n'
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
    # When an already-installed package (gtk4) would upgrade, fail cleanly without omarchy update or -Syu.
    if (resolve_and_install_pacman_target -S super-desktop-bin </dev/null) 2>"$WORK_DIR/err-partial"; then
        echo "Expected partial upgrade guard to refuse upgrading gtk4" >&2
        exit 1
    fi
    grep -q 'gtk4' "$WORK_DIR/err-partial"
    grep -q 'does not run a full system upgrade' "$WORK_DIR/err-partial"
    ! grep -q '^omarchy update$' "$WORK_DIR/calls-partial.log"
    ! grep -q -- '-Syu' "$WORK_DIR/calls-partial.log"
    # Verify single-repo sync config preserves [options] and excludes [omarchy].
    grep -q '^DownloadUser = alpm$' "$WORK_DIR/synced-config-partial.conf"
    grep -qxF "Include = $PACMAN_REPO_CONF" "$WORK_DIR/synced-config-partial.conf"
    ! grep -q '^\[omarchy\]' "$WORK_DIR/synced-config-partial.conf"

    # When only new dependencies (vte4) and super-desktop-bin are needed, install cleanly.
    touch "$WORK_DIR/clean-partial"
    resolve_and_install_pacman_target -S super-desktop-bin </dev/null
    grep -q '^sudo pacman -S --needed --noconfirm super-desktop-bin$' "$WORK_DIR/calls-partial.log"
    ! grep -q '^omarchy update$' "$WORK_DIR/calls-partial.log"
    ! grep -q -- '-Syu' "$WORK_DIR/calls-partial.log"
}
run_case partial case_partial_upgrade_guard

# 4b. Standalone docs/install/pacman-repo.sh syncs only [super-desktop] and never runs -Syu.
case_standalone_pacman_repo_script() {
    source "$WORK_DIR/pacman_repo_lib.sh"
    pacman() {
        printf 'pacman %s\n' "$*" >>"$WORK_DIR/calls-standalone.log"
        if [[ "$1" == "--config" ]]; then
            cp "$2" "$WORK_DIR/synced-config-standalone.conf"
            shift 2
        fi
        case "$1" in
        -Sy | -S) return 0 ;;
        -Sp)
            if [[ -f "$WORK_DIR/standalone-conflict" ]]; then
                echo ":: installing gstreamer (1.28.9-3) breaks dependency 'gstreamer=1.28.9-1' required by gst-plugins-bad-libs" >&2
                return 1
            fi
            if [[ "$*" == *"--print-format %n"* ]]; then
                if [[ -f "$WORK_DIR/standalone-upgrade" ]]; then
                    printf 'gtk4\nsuper-desktop-bin\n'
                else
                    printf 'vte4\nsuper-desktop-bin\n'
                fi
            fi
            return 0
            ;;
        -Q)
            [[ "$2" == "gtk4" ]]
            ;;
        esac
    }
    configure_repo

    # Conflict fails cleanly without -Syu.
    touch "$WORK_DIR/standalone-conflict"
    if (install_package </dev/null) 2>"$WORK_DIR/err-standalone-conflict"; then
        echo "Expected standalone pacman-repo.sh to fail on dependency conflict" >&2
        exit 1
    fi
    grep -q 'without a system upgrade' "$WORK_DIR/err-standalone-conflict"
    rm -f "$WORK_DIR/standalone-conflict"

    # Partial upgrade fails cleanly without -Syu.
    touch "$WORK_DIR/standalone-upgrade"
    if (install_package </dev/null) 2>"$WORK_DIR/err-standalone-upgrade"; then
        echo "Expected standalone pacman-repo.sh to refuse upgrading gtk4" >&2
        exit 1
    fi
    grep -q 'gtk4' "$WORK_DIR/err-standalone-upgrade"
    rm -f "$WORK_DIR/standalone-upgrade"

    # Clean install succeeds via pacman -S --needed without -Syu.
    install_package </dev/null
    grep -q '^DownloadUser = alpm$' "$WORK_DIR/synced-config-standalone.conf"
    grep -qxF "Include = $PACMAN_REPO_CONF" "$WORK_DIR/synced-config-standalone.conf"
    ! grep -q '^\[omarchy\]' "$WORK_DIR/synced-config-standalone.conf"
    grep -q '^sudo pacman -S --needed --noconfirm super-desktop-bin$' "$WORK_DIR/calls-standalone.log"
    ! grep -q -- '-Syu' "$WORK_DIR/calls-standalone.log"
}
run_case standalone case_standalone_pacman_repo_script

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

# 6. Signing key fingerprint matches across fingerprint file and all installers.
case_fingerprint_consistency() {
    local expected fpr_install fpr_repo fpr_direct
    expected="$(tr -d '[:space:]' <packaging/arch/super-desktop.gpg.fingerprint)"
    [[ "$expected" =~ ^[0-9A-F]{40}$ ]]
    fpr_install="$(sed -n 's/^REPO_KEY_FINGERPRINT="\([^"]*\)"$/\1/p' install.sh)"
    fpr_repo="$(sed -n 's/^REPO_FPR="\([^"]*\)"$/\1/p' docs/install/pacman-repo.sh)"
    fpr_direct="$(sed -n 's/^REPO_FPR="\([^"]*\)"$/\1/p' docs/install/direct-pkg.sh)"
    [[ "$fpr_install" == "$expected" ]]
    [[ "$fpr_repo" == "$expected" ]]
    [[ "$fpr_direct" == "$expected" ]]
}
case_fingerprint_consistency

# 7. Standalone docs/install/direct-pkg.sh exits 0 and cleans temp dirs (no unbound work trap).
case_standalone_direct_pkg() {
    local expected arch mock_bin mock_home mock_tmp out
    expected="$(tr -d '[:space:]' <packaging/arch/super-desktop.gpg.fingerprint)"
    arch="$(uname -m)"
    mock_bin="$WORK_DIR/direct-bin"
    mock_home="$WORK_DIR/direct-home"
    mock_tmp="$WORK_DIR/direct-tmp"
    mkdir -p "$mock_bin" "$mock_home" "$mock_tmp"

    local pkg_name="super-desktop-bin-2.2.0-1-${arch}.pkg.tar.zst"
    printf 'dummy-pkg\n' >"$WORK_DIR/$pkg_name"
    local sha
    sha="$(sha256sum "$WORK_DIR/$pkg_name" | awk '{ print $1 }')"
    printf '%s  %s\n' "$sha" "$pkg_name" >"$WORK_DIR/direct-SHA256SUMS"
    printf 'sig\n' >"$WORK_DIR/$pkg_name.sig"
    printf '{"tag_name": "v2.2.0"}\n' >"$WORK_DIR/latest.json"

    cat >"$mock_bin/curl" <<EOF
#!/usr/bin/env bash
out=""
url=""
while ((\$# > 0)); do
    case "\$1" in
    -o) out="\$2"; shift 2 ;;
    --proto|--retry) shift 2 ;;
    -*) shift ;;
    *) url="\$1"; shift ;;
    esac
done
case "\$url" in
*releases/latest) cat "$WORK_DIR/latest.json" ;;
*.pkg.tar.zst.sig) cp "$WORK_DIR/$pkg_name.sig" "\$out" ;;
*.pkg.tar.zst) cp "$WORK_DIR/$pkg_name" "\$out" ;;
*SHA256SUMS) cp "$WORK_DIR/direct-SHA256SUMS" "\$out" ;;
*super-desktop.gpg) cp "$WORK_DIR/super-desktop.gpg" "\$out" ;;
*) exit 22 ;;
esac
EOF
    cat >"$mock_bin/sudo" <<'EOF'
#!/usr/bin/env bash
"$@"
EOF
    cat >"$mock_bin/pacman" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--config" ]]; then
    shift 2
fi
case "${1:-}" in
-Sp)
    if [[ "$*" == *"--print-format %n"* ]]; then
        printf 'super-desktop-bin\n'
    fi
    exit 0
    ;;
-Q)
    exit 1
    ;;
*)
    exit 0
    ;;
esac
EOF
    cat >"$mock_bin/pacman-key" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
--list-keys) exit 1 ;;
*) exit 0 ;;
esac
EOF
    cat >"$mock_bin/gpg" <<EOF
#!/usr/bin/env bash
printf 'pub:-:255:22:%s:1700000000:::-:::scESC::::::23::0:\n' "$expected"
printf 'fpr:::::::::%s:\n' "$expected"
EOF
    cat >"$mock_bin/super-desktop-setup" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
    chmod +x "$mock_bin"/*

    out="$(
        env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$mock_home" TMPDIR="$mock_tmp" \
            SUPER_DESKTOP_SETUP_BIN="$mock_bin/super-desktop-setup" \
            SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo" \
            SUPER_DESKTOP_RELEASE_BASE_URL="https://example.invalid/releases/download" \
            SUPER_DESKTOP_RELEASE_LATEST_API="https://example.invalid/releases/latest" \
            bash docs/install/direct-pkg.sh 2>&1
    )"
    grep -qF "✓ Installed $pkg_name!" <<<"$out"
    ! grep -qF "unbound variable" <<<"$out"
    [[ -z "$(ls -A "$mock_tmp")" ]]

    # Also test explicit VER=2.2.0
    out="$(
        env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$mock_home" TMPDIR="$mock_tmp" \
            VER=2.2.0 \
            SUPER_DESKTOP_SETUP_BIN="$mock_bin/super-desktop-setup" \
            SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo" \
            SUPER_DESKTOP_RELEASE_BASE_URL="https://example.invalid/releases/download" \
            bash docs/install/direct-pkg.sh 2>&1
    )"
    grep -qF "✓ Installed $pkg_name!" <<<"$out"
    [[ -z "$(ls -A "$mock_tmp")" ]]
}
case_standalone_direct_pkg

# 8. Standalone docs/install/pacman-repo.sh and pacman-hook.sh configure repo, clean TMPDIR, and exit 0.
case_standalone_pacman_repo_and_hook() {
    local expected mock_bin mock_home mock_tmp pconf rconf
    expected="$(tr -d '[:space:]' <packaging/arch/super-desktop.gpg.fingerprint)"
    mock_bin="$WORK_DIR/repo-bin"
    mock_home="$WORK_DIR/repo-home"
    mock_tmp="$WORK_DIR/repo-tmp"
    pconf="$WORK_DIR/repo-pacman.conf"
    rconf="$WORK_DIR/repo-pacman.d/super-desktop.conf"
    mkdir -p "$mock_bin" "$mock_home" "$mock_tmp"
    printf '[omarchy]\nServer = https://pkgs.omarchy.org/stable/$arch\n' >"$pconf"

    cat >"$mock_bin/curl" <<EOF
#!/usr/bin/env bash
out=""
while ((\$# > 0)); do
    if [[ "\$1" == "-o" ]]; then out="\$2"; shift 2; else shift; fi
done
cp "$WORK_DIR/super-desktop.gpg" "\$out"
EOF
    cat >"$mock_bin/gpg" <<EOF
#!/usr/bin/env bash
printf 'pub:-:255:22:%s:1700000000:::-:::scESC::::::23::0:\n' "$expected"
printf 'fpr:::::::::%s:\n' "$expected"
EOF
    cat >"$mock_bin/sudo" <<'EOF'
#!/usr/bin/env bash
case "$1" in
mkdir) mkdir -p "${@:3}" ;;
install)
    mkdir -p "$(dirname "$5")"
    cp "$4" "$5"
    chmod "$3" "$5"
    ;;
*) "$@" ;;
esac
EOF
    cat >"$mock_bin/pacman" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--config" ]]; then
    shift 2
fi
case "${1:-}" in
-Sp)
    if [[ "$*" == *"--print-format %n"* ]]; then
        printf 'super-desktop-bin\n'
    fi
    exit 0
    ;;
-Q)
    exit 1
    ;;
*)
    exit 0
    ;;
esac
EOF
    cat >"$mock_bin/pacman-key" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
--list-keys) exit 1 ;;
*) exit 0 ;;
esac
EOF
    cat >"$mock_bin/super-desktop-setup" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
    chmod +x "$mock_bin"/*

    env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$mock_home" TMPDIR="$mock_tmp" \
        SUPER_DESKTOP_PACMAN_CONF="$pconf" \
        SUPER_DESKTOP_PACMAN_REPO_CONF="$rconf" \
        SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo" \
        SUPER_DESKTOP_SETUP_BIN="$mock_bin/super-desktop-setup" \
        bash docs/install/pacman-repo.sh >/dev/null

    [[ -f "$rconf" ]]
    grep -qxF "Include = $rconf" "$pconf"
    [[ -x "$mock_home/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop" ]]
    [[ -z "$(ls -A "$mock_tmp")" ]]

    rm -f "$mock_home/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"
    env -i PATH="/usr/bin:/bin" HOME="$mock_home" bash docs/install/pacman-hook.sh >/dev/null
    [[ -x "$mock_home/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop" ]]
}
case_standalone_pacman_repo_and_hook

# 9. source.sh and master.sh fail loudly when curl fails, and forward expected env vars.
case_source_and_master_wrappers() {
    local fail_bin pass_bin out
    fail_bin="$WORK_DIR/wrap-fail"
    pass_bin="$WORK_DIR/wrap-pass"
    mkdir -p "$fail_bin" "$pass_bin"

    cat >"$fail_bin/curl" <<'EOF'
#!/usr/bin/env bash
exit 22
EOF
    chmod +x "$fail_bin/curl"

    for script in docs/install/source.sh docs/install/master.sh; do
        if env -i PATH="$fail_bin:/usr/bin:/bin" HOME="$WORK_DIR" bash "$script" >/dev/null 2>&1; then
            echo "Expected $script to exit non-zero when curl fails" >&2
            exit 1
        fi
    done

    cat >"$pass_bin/curl" <<'EOF'
#!/usr/bin/env bash
cat <<'INNER'
printf 'BUILD=%s VER=%s\n' "${SUPER_DESKTOP_BUILD_FROM_SOURCE:-}" "${SUPER_DESKTOP_VERSION:-}"
INNER
EOF
    chmod +x "$pass_bin/curl"

    out="$(env -i PATH="$pass_bin:/usr/bin:/bin" HOME="$WORK_DIR" bash docs/install/source.sh)"
    [[ "$out" == "BUILD=1 VER=" ]]
    out="$(env -i PATH="$pass_bin:/usr/bin:/bin" HOME="$WORK_DIR" bash docs/install/master.sh)"
    [[ "$out" == "BUILD= VER=master" ]]
}
case_source_and_master_wrappers

# 10. detect_distro routes Omarchy, plain Arch, Fedora, Ubuntu, Debian, and openSUSE cleanly.
case_detect_distro_routing() {
    local osrel mock_bin
    for spec in \
        "omarchy:omarchy:Omarchy::pacman:omarchy:1" \
        "arch:arch:Arch Linux::pacman:arch:0" \
        "fedora:fedora:Fedora Linux 45::dnf:fedora:0" \
        "ubuntu:ubuntu:Ubuntu 26.04.1 LTS:debian:apt-get:debian:0" \
        "debian:debian:Debian GNU/Linux forky/sid::apt-get:debian:0" \
        "opensuse:opensuse-tumbleweed:openSUSE Tumbleweed:opensuse suse:zypper:opensuse:0"; do
        IFS=':' read -r case_id os_id pretty like pm want_fam want_omarchy <<<"$spec"
        osrel="$WORK_DIR/os-release-$case_id"
        mock_bin="$WORK_DIR/pm-$case_id"
        mkdir -p "$mock_bin"
        cat >"$osrel" <<EOF
ID=$os_id
ID_LIKE="$like"
PRETTY_NAME="$pretty"
EOF
        printf '#!/bin/sh\nexit 0\n' >"$mock_bin/$pm"
        chmod +x "$mock_bin/$pm"
        if [[ "$case_id" == "omarchy" ]]; then
            printf '#!/bin/sh\nexit 0\n' >"$mock_bin/omarchy"
            chmod +x "$mock_bin/omarchy"
        fi
        local force_flag=1
        [[ "$case_id" == "omarchy" ]] && force_flag=0
        out="$(
            env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$WORK_DIR/home-$case_id" \
                SUPER_DESKTOP_OS_RELEASE="$osrel" \
                SUPER_DESKTOP_FORCE_NO_OMARCHY="$force_flag" \
                bash -euo pipefail -c '
                    source "$1"
                    detect_distro >/dev/null
                    printf "%s:%s\n" "$DISTRO_FAMILY" "$IS_OMARCHY"
                ' _ "$WORK_DIR/install_lib.sh"
        )"
        [[ "$out" == "$want_fam:$want_omarchy" ]] || {
            echo "Unexpected detect_distro for $case_id: got '$out', expected '$want_fam:$want_omarchy'" >&2
            exit 1
        }
    done
}
case_detect_distro_routing

# 11. configure_non_omarchy_compositor updates hyprland.conf idempotently or preserves absence.
case_non_omarchy_compositor() {
    local home_with="$WORK_DIR/home-hyprconf" home_without="$WORK_DIR/home-nohypr"
    mkdir -p "$home_with/.config/hypr" "$home_without"
    printf 'monitor=,preferred,auto,1\n' >"$home_with/.config/hypr/hyprland.conf"

    HOME="$home_with" bash -euo pipefail -c '
        source "$1"
        configure_non_omarchy_compositor "/usr/bin/super-desktop" >/dev/null
        configure_non_omarchy_compositor "/usr/bin/super-desktop" >/dev/null
        test "$NON_OMARCHY_NEEDS_BINDING" -eq 0
    ' _ "$WORK_DIR/install_lib.sh"
    [[ -f "$home_with/.config/hypr/hyprland.conf.before-super-desktop" ]]
    [[ "$(grep -cF '# >>> super-desktop >>>' "$home_with/.config/hypr/hyprland.conf")" -eq 1 ]]
    grep -qF 'exec-once = /usr/bin/super-desktop daemon' "$home_with/.config/hypr/hyprland.conf"
    grep -qF 'bindr = SUPER SHIFT, Q, exec, /usr/bin/super-desktop toggle' "$home_with/.config/hypr/hyprland.conf"

    HOME="$home_without" bash -euo pipefail -c '
        source "$1"
        configure_non_omarchy_compositor "/usr/bin/super-desktop" >/dev/null
        test "$NON_OMARCHY_NEEDS_BINDING" -eq 1
    ' _ "$WORK_DIR/install_lib.sh"
    [[ ! -e "$home_without/.config/hypr/hyprland.conf" ]]
}
case_non_omarchy_compositor

# 12. Plain Arch pacman-repo.sh and direct-pkg.sh exit 0 cleanly when hyprland.lua is absent.
case_plain_arch_standalone_scripts() {
    local expected mock_bin mock_home pconf rconf
    expected="$(tr -d '[:space:]' <packaging/arch/super-desktop.gpg.fingerprint)"
    mock_bin="$WORK_DIR/plain-arch-bin"
    mock_home="$WORK_DIR/plain-arch-home"
    pconf="$WORK_DIR/plain-pacman.conf"
    rconf="$WORK_DIR/plain-pacman.d/super-desktop.conf"
    mkdir -p "$mock_bin" "$mock_home"
    printf '[core]\nInclude = /etc/pacman.d/mirrorlist\n' >"$pconf"

    local arch pkg_name sha
    arch="$(uname -m)"
    pkg_name="super-desktop-bin-2.2.0-1-${arch}.pkg.tar.zst"
    printf 'dummy-pkg\n' >"$WORK_DIR/$pkg_name"
    sha="$(sha256sum "$WORK_DIR/$pkg_name" | awk '{ print $1 }')"
    printf '%s  %s\n' "$sha" "$pkg_name" >"$WORK_DIR/plain-SHA256SUMS"
    printf 'sig\n' >"$WORK_DIR/$pkg_name.sig"

    cat >"$mock_bin/curl" <<EOF
#!/usr/bin/env bash
out=""
url=""
while ((\$# > 0)); do
    case "\$1" in
    -o) out="\$2"; shift 2 ;;
    --proto|--retry) shift 2 ;;
    -*) shift ;;
    *) url="\$1"; shift ;;
    esac
done
case "\$url" in
*.pkg.tar.zst.sig) cp "$WORK_DIR/$pkg_name.sig" "\$out" ;;
*.pkg.tar.zst) cp "$WORK_DIR/$pkg_name" "\$out" ;;
*SHA256SUMS) cp "$WORK_DIR/plain-SHA256SUMS" "\$out" ;;
*super-desktop.gpg) cp "$WORK_DIR/super-desktop.gpg" "\$out" ;;
*) exit 22 ;;
esac
EOF
    cat >"$mock_bin/gpg" <<EOF
#!/usr/bin/env bash
printf 'pub:-:255:22:%s:1700000000:::-:::scESC::::::23::0:\n' "$expected"
printf 'fpr:::::::::%s:\n' "$expected"
EOF
    cat >"$mock_bin/sudo" <<'EOF'
#!/usr/bin/env bash
case "$1" in
mkdir) mkdir -p "${@:3}" ;;
install)
    mkdir -p "$(dirname "$5")"
    cp "$4" "$5"
    chmod "$3" "$5"
    ;;
*) "$@" ;;
esac
EOF
    cat >"$mock_bin/pacman" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--config" ]]; then
    shift 2
fi
case "${1:-}" in
-Sp)
    if [[ "$*" == *"--print-format %n"* ]]; then
        printf 'super-desktop-bin\n'
    fi
    exit 0
    ;;
-Q)
    exit 1
    ;;
*)
    exit 0
    ;;
esac
EOF
    cat >"$mock_bin/pacman-key" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
--list-keys) exit 1 ;;
*) exit 0 ;;
esac
EOF
    chmod +x "$mock_bin"/*

    env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$mock_home" \
        SUPER_DESKTOP_PACMAN_CONF="$pconf" \
        SUPER_DESKTOP_PACMAN_REPO_CONF="$rconf" \
        SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo" \
        bash docs/install/pacman-repo.sh >/dev/null

    env -i PATH="$mock_bin:/usr/bin:/bin" HOME="$mock_home" \
        VER=2.2.0 \
        SUPER_DESKTOP_PACMAN_REPO_URL="https://example.invalid/repo" \
        SUPER_DESKTOP_RELEASE_BASE_URL="https://example.invalid/releases/download" \
        bash docs/install/direct-pkg.sh >/dev/null
}
case_plain_arch_standalone_scripts

echo "Installer binary and repo checks passed."
