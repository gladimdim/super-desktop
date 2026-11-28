#!/usr/bin/env bash
# End-to-end "Omarchy from scratch" release verification suite.
#
# Builds SUPER DESKTOP inside a clean Omarchy stable container (or uses prebuilt
# release assets / signed Pacman repo) and tests every installer script inside
# fresh ephemeral Omarchy stable containers:
#   1. install.sh (binary-first Pacman repo, idempotency, pre-refresh-pacman hook,
#      pacman -Syu, ldd, headless Broadway daemon lifecycle, sd-qml-host)
#   2. docs/install/pacman-repo.sh + docs/install/pacman-hook.sh
#   3. docs/install/direct-pkg.sh (auto-latest + explicit VER, TMPDIR cleanup)
#   4. install.sh pinned release (SUPER_DESKTOP_VERSION=vX.Y.Z)
#   5. docs/install/source.sh (Rust source build + sd-qml-host + Broadway smoke)
#      followed by migration to super-desktop-bin via install.sh
#   6. docs/install/master.sh (branch source build + sd-qml-host)
#
# Usage:
#   tests/omarchy_fresh_release_test.sh [options]
#
# Options:
#   --tag vX.Y.Z            Test exact annotated git tag vX.Y.Z
#   --head                  Test current working tree as a scratch tag matching
#                           Cargo.toml (default when --tag is omitted)
#   --arch ARCH             x86_64 or aarch64 (default: host uname -m)
#   --release-assets DIR    Use pre-built release-assets dir (requires --tag)
#   --repo-dir DIR          Use pre-built signed Pacman repo dir for --arch
#                           (with --release-assets)
#   --src-dir DIR           Use existing git checkout for the bare git mirror
#   --key-file PATH         GPG private key for signing when building from
#                           scratch (uses an ephemeral Ed25519 key if omitted)
#   --flows SPEC            all (default), pacman, or source
#   --engine NAME           podman or docker (default: auto-detect)
#   --no-cache              Pass --no-cache to container builds
#   -h, --help              Show this help text
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TAG=""
USE_HEAD=0
ARCH="$(uname -m)"
RELEASE_ASSETS=""
REPO_DIR=""
SRC_DIR=""
KEY_FILE=""
FLOWS="all"
ENGINE=""
NO_CACHE=0

usage() {
    sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'
}

while (($# > 0)); do
    case "$1" in
    --tag)
        TAG="${2:?Missing value for --tag}"
        shift 2
        ;;
    --head)
        USE_HEAD=1
        shift
        ;;
    --arch)
        ARCH="${2:?Missing value for --arch}"
        shift 2
        ;;
    --release-assets)
        RELEASE_ASSETS="${2:?Missing value for --release-assets}"
        shift 2
        ;;
    --repo-dir)
        REPO_DIR="${2:?Missing value for --repo-dir}"
        shift 2
        ;;
    --src-dir)
        SRC_DIR="${2:?Missing value for --src-dir}"
        shift 2
        ;;
    --key-file)
        KEY_FILE="${2:?Missing value for --key-file}"
        shift 2
        ;;
    --flows)
        FLOWS="${2:?Missing value for --flows}"
        shift 2
        ;;
    --engine)
        ENGINE="${2:?Missing value for --engine}"
        shift 2
        ;;
    --no-cache)
        NO_CACHE=1
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "Error: Unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
done

case "$ARCH" in
x86_64 | aarch64) ;;
*)
    echo "Error: Unsupported --arch '$ARCH' (expected x86_64 or aarch64)" >&2
    exit 1
    ;;
esac

case "$FLOWS" in
all | pacman | source) ;;
*)
    echo "Error: Unsupported --flows '$FLOWS' (expected all, pacman, or source)" >&2
    exit 1
    ;;
esac

if [[ -n "$TAG" && "$USE_HEAD" -eq 1 ]]; then
    echo "Error: --tag and --head are mutually exclusive" >&2
    exit 1
fi
if [[ -n "$RELEASE_ASSETS" && -z "$TAG" ]]; then
    echo "Error: --release-assets requires --tag vX.Y.Z" >&2
    exit 1
fi
if [[ -n "$REPO_DIR" && -z "$RELEASE_ASSETS" ]]; then
    echo "Error: --repo-dir requires --release-assets" >&2
    exit 1
fi
if [[ -z "$TAG" ]]; then
    USE_HEAD=1
fi

if [[ -z "$ENGINE" ]]; then
    if command -v podman >/dev/null 2>&1; then
        ENGINE="podman"
    elif command -v docker >/dev/null 2>&1; then
        ENGINE="docker"
    else
        echo "Error: Neither podman nor docker was found in PATH" >&2
        exit 1
    fi
fi
case "$ENGINE" in
podman | docker)
    command -v "$ENGINE" >/dev/null 2>&1 || {
        echo "Error: Container engine '$ENGINE' not found in PATH" >&2
        exit 1
    }
    ;;
*)
    echo "Error: --engine must be 'podman' or 'docker'" >&2
    exit 1
    ;;
esac

oci_platform_for() {
    case "$1" in
    x86_64) echo "linux/amd64" ;;
    aarch64) echo "linux/arm64" ;;
    *) return 1 ;;
    esac
}
OCI_PLATFORM="$(oci_platform_for "$ARCH")"

HOST_ARCH="$(uname -m)"
if [[ "$ARCH" != "$HOST_ARCH" ]]; then
    binfmt_file="/proc/sys/fs/binfmt_misc/qemu-${ARCH}"
    if [[ ! -r "$binfmt_file" ]] || ! head -n1 "$binfmt_file" | grep -qx 'enabled'; then
        echo "Error: Cross-arch testing for $ARCH on $HOST_ARCH requires enabled binfmt_misc handler ($binfmt_file)." >&2
        exit 1
    fi
fi

for cmd in git python3 openssl tar sha256sum; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "Error: Required host tool missing: $cmd" >&2
        exit 1
    }
done

WORK="$(mktemp -d)"
cleanup() {
    rm -rf "$WORK"
}
trap cleanup EXIT

STAGE_SRC=""
if [[ -n "$SRC_DIR" ]]; then
    SRC_DIR="$(cd "$SRC_DIR" && pwd)"
    [[ -f "$SRC_DIR/Cargo.toml" ]] || {
        echo "Error: --src-dir does not contain Cargo.toml: $SRC_DIR" >&2
        exit 1
    }
    STAGE_SRC="$SRC_DIR"
else
    STAGE_SRC="$WORK/src"
    if ((USE_HEAD == 1)); then
        VER="$(python3 -c 'import pathlib, sys, tomllib; print(tomllib.loads((pathlib.Path(sys.argv[1]) / "Cargo.toml").read_text())["package"]["version"])' "$ROOT")"
        [[ "$VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
            echo "Error: Invalid Cargo.toml version '$VER'" >&2
            exit 1
        }
        TAG="v$VER"
        echo "==> Staging current working tree as scratch tag $TAG..."
        mkdir -p "$STAGE_SRC"
        python3 - "$ROOT" "$STAGE_SRC" <<'PY'
import os, shutil, subprocess, sys
from pathlib import Path
src = Path(sys.argv[1])
dst = Path(sys.argv[2])
raw = subprocess.check_output(
    ["git", "-C", str(src), "ls-files", "-z", "--cached", "--others", "--exclude-standard"]
)
for rel_b in raw.split(b"\0"):
    if not rel_b:
        continue
    rel = rel_b.decode("utf-8")
    sp = src / rel
    if not os.path.lexists(sp):
        continue
    dp = dst / rel
    dp.parent.mkdir(parents=True, exist_ok=True)
    if os.path.islink(sp):
        if os.path.lexists(dp):
            dp.unlink()
        os.symlink(os.readlink(sp), dp)
    elif os.path.isfile(sp):
        shutil.copy2(sp, dp)
PY
        git -C "$STAGE_SRC" init -q -b master
        git -C "$STAGE_SRC" config user.name "SUPER DESKTOP Test"
        git -C "$STAGE_SRC" config user.email "test@example.com"
        git -C "$STAGE_SRC" add -A
        git -C "$STAGE_SRC" commit -q -m "Test $TAG"
        git -C "$STAGE_SRC" tag -a "$TAG" -m "SUPER DESKTOP $VER"
    else
        [[ "$TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || {
            echo "Error: --tag must look like vX.Y.Z" >&2
            exit 1
        }
        VER="${BASH_REMATCH[1]}"
        if [[ "$(git -C "$ROOT" cat-file -t "refs/tags/$TAG" 2>/dev/null || true)" != "tag" ]]; then
            echo "Error: refs/tags/$TAG does not exist or is not an annotated git tag" >&2
            exit 1
        fi
        echo "==> Exporting exact annotated tag $TAG..."
        git clone --no-local --branch "$TAG" "file://$ROOT" "$STAGE_SRC" >/dev/null 2>&1
    fi
fi

[[ "$TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || {
    echo "Error: Tag '$TAG' must look like vX.Y.Z" >&2
    exit 1
}
VER="${BASH_REMATCH[1]}"
grep -qxF "version = \"$VER\"" "$STAGE_SRC/Cargo.toml" || {
    echo "Error: $TAG Cargo.toml version does not match $VER in $STAGE_SRC" >&2
    exit 1
}
chmod -R a+rX "$STAGE_SRC" 2>/dev/null || true

PKG_NAME="super-desktop-bin-${VER}-1-${ARCH}.pkg.tar.zst"
STAGED_ASSETS="$WORK/release-assets"
STAGED_REPO="$WORK/repo/$ARCH"
mkdir -p "$STAGED_ASSETS" "$STAGED_REPO"

EPHEMERAL_KEY=0
SIGN_FPR=""

if [[ -n "$RELEASE_ASSETS" ]]; then
    RELEASE_ASSETS="$(cd "$RELEASE_ASSETS" && pwd)"
    [[ -r "$RELEASE_ASSETS/$PKG_NAME" && -r "$RELEASE_ASSETS/SHA256SUMS" ]] || {
        echo "Error: Prebuilt assets missing $PKG_NAME or SHA256SUMS in $RELEASE_ASSETS" >&2
        exit 1
    }
    cp -f "$RELEASE_ASSETS/$PKG_NAME" "$STAGED_ASSETS/"
    cp -f "$RELEASE_ASSETS/SHA256SUMS" "$STAGED_ASSETS/"
    if [[ -r "$RELEASE_ASSETS/$PKG_NAME.sig" ]]; then
        cp -f "$RELEASE_ASSETS/$PKG_NAME.sig" "$STAGED_ASSETS/"
    fi
    if [[ -n "$REPO_DIR" ]]; then
        REPO_DIR="$(cd "$REPO_DIR" && pwd)"
        [[ -r "$REPO_DIR/super-desktop.db" && -r "$REPO_DIR/super-desktop.db.sig" ]] || {
            echo "Error: --repo-dir missing signed super-desktop.db/.sig: $REPO_DIR" >&2
            exit 1
        }
        cp -a "$REPO_DIR/." "$STAGED_REPO/"
        cp -f "$ROOT/packaging/arch/super-desktop.gpg" "$WORK/super-desktop.gpg"
        SIGN_FPR="$(tr -d '[:space:]' <"$ROOT/packaging/arch/super-desktop.gpg.fingerprint")"
    elif [[ "$FLOWS" != "source" ]]; then
        for cmd in gpg repo-add; do
            command -v "$cmd" >/dev/null 2>&1 || {
                echo "Error: Signing an ephemeral Pacman repo requires $cmd (or pass --repo-dir)" >&2
                exit 1
            }
        done
        EPHEMERAL_KEY=1
    fi
else
    builder_tag="sd-arch-builder:${ARCH}"
    builder_out="$WORK/builder-out"
    mkdir -p "$builder_out"
    chmod 755 "$builder_out"

    echo "==> Building builder container $builder_tag ($OCI_PLATFORM)..."
    b_args=(build --platform "$OCI_PLATFORM" -t "$builder_tag")
    ((NO_CACHE == 0)) || b_args+=(--no-cache)
    b_args+=("$ROOT/packaging/arch/builder")
    "$ENGINE" "${b_args[@]}"

    echo "==> Building, testing, and packaging $TAG from scratch in $builder_tag..."
    "$ENGINE" run --rm \
        --platform "$OCI_PLATFORM" \
        -e "RELEASE_TAG=$TAG" \
        -v "$STAGE_SRC:/src:ro" \
        -v "$builder_out:/out" \
        "$builder_tag" \
        bash -euo pipefail -c '
            trap "chown -R 0:0 /out 2>/dev/null || true; chmod -R a+rX /out 2>/dev/null || true" EXIT
            cp -a /src /work
            chown -R builder:builder /work /out
            runuser -u builder -- bash -c "cd /work && bash scripts/build-arch-release.sh \"\$1\" /out" _ "$RELEASE_TAG"
            pacman -U --noconfirm /out/*.pkg.tar.zst
            touch /super-desktop-package-test-root
            install -d -m 700 -o builder -g builder /tmp/sd-package-runtime
            runuser -u builder -- env -u SD_GTK_TESTS_ON_DESKTOP XDG_RUNTIME_DIR=/tmp/sd-package-runtime \
                bash -c "cd /work && cargo test --locked package_tests::packaged_desktop_lifecycle -- --ignored --nocapture"
        '

    [[ -r "$builder_out/$PKG_NAME" ]] || {
        echo "Error: Builder did not produce $PKG_NAME" >&2
        exit 1
    }
    cp -f "$builder_out"/*.tar.gz "$STAGED_ASSETS/"
    cp -f "$builder_out/$PKG_NAME" "$STAGED_ASSETS/"

    if [[ "$FLOWS" != "source" ]]; then
        for cmd in gpg repo-add; do
            command -v "$cmd" >/dev/null 2>&1 || {
                echo "Error: Required host tool missing for Pacman repo creation: $cmd" >&2
                exit 1
            }
        done
    fi
fi

if [[ "$FLOWS" != "source" && ! -f "$STAGED_REPO/super-desktop.db" ]]; then
    GNUPG_WORK="$WORK/gnupg"
    mkdir -p "$GNUPG_WORK"
    chmod 700 "$GNUPG_WORK"
    export GNUPGHOME="$GNUPG_WORK"

    if [[ -n "$KEY_FILE" ]]; then
        gpg --batch --quiet --import "$KEY_FILE"
        SIGN_FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
        expected="$(tr -d '[:space:]' <"$ROOT/packaging/arch/super-desktop.gpg.fingerprint")"
        [[ "$SIGN_FPR" == "$expected" ]] || {
            echo "Error: Key fingerprint $SIGN_FPR does not match $expected" >&2
            exit 1
        }
        cp -f "$ROOT/packaging/arch/super-desktop.gpg" "$WORK/super-desktop.gpg"
    else
        EPHEMERAL_KEY=1
        gpg --batch --gen-key <<'EOF'
Key-Type: eddsa
Key-Curve: ed25519
Key-Usage: sign
Name-Real: SUPER DESKTOP Fresh Test Signing
Name-Email: test@example.com
Expire-Date: 7d
%no-protection
%commit
EOF
        SIGN_FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
        [[ "$SIGN_FPR" =~ ^[0-9A-F]{40}$ ]]
        gpg --armor --export "$SIGN_FPR" >"$WORK/super-desktop.gpg"
    fi

    bash "$ROOT/scripts/build-pacman-repo.sh" \
        --repo-name super-desktop \
        --key-fingerprint "$SIGN_FPR" \
        --output "$STAGED_REPO" \
        "$STAGED_ASSETS/$PKG_NAME"
    cp -f "$STAGED_REPO/$PKG_NAME.sig" "$STAGED_ASSETS/"
    (
        cd "$STAGED_ASSETS"
        sha256sum *.tar.gz *.pkg.tar.zst >SHA256SUMS
    )
    unset GNUPGHOME
fi

# Stage the self-contained HTTPS fixture served on 127.0.0.1:8443 inside each test container.
FIXTURE="$WORK/fixture"
SITE="$FIXTURE/site"
mkdir -p "$FIXTURE/tls" "$SITE/install" "$SITE/repo/$ARCH" "$SITE/releases/download/$TAG" "$SITE/api/releases" "$SITE/git"

openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
    -keyout "$FIXTURE/tls/ca.key" -out "$FIXTURE/tls/ca.crt" \
    -subj "/CN=SUPER DESKTOP Test CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null

openssl req -newkey rsa:2048 -nodes \
    -keyout "$FIXTURE/tls/server.key" -out "$FIXTURE/tls/server.csr" \
    -subj "/CN=127.0.0.1" 2>/dev/null

cat >"$FIXTURE/tls/ext.cnf" <<'EOF'
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=IP:127.0.0.1,DNS:localhost
EOF

openssl x509 -req -in "$FIXTURE/tls/server.csr" \
    -CA "$FIXTURE/tls/ca.crt" -CAkey "$FIXTURE/tls/ca.key" -CAcreateserial \
    -out "$FIXTURE/tls/server.crt" -days 1 -extfile "$FIXTURE/tls/ext.cnf" 2>/dev/null

cp -f "$ROOT/install.sh" "$SITE/install/install.sh"
cp -f "$ROOT"/docs/install/*.sh "$SITE/install/"
chmod 755 "$SITE/install"/*.sh

if ((EPHEMERAL_KEY == 1)); then
    echo "==> Using ephemeral GPG key ($SIGN_FPR) in staged installer copies..."
    python3 - "$SIGN_FPR" "$SITE/install/install.sh" "$SITE/install/pacman-repo.sh" "$SITE/install/direct-pkg.sh" <<'PY'
import re, sys
from pathlib import Path
fpr = sys.argv[1]
for p_str in sys.argv[2:]:
    p = Path(p_str)
    text = p.read_text()
    text, n = re.subn(r'^(REPO_KEY_FINGERPRINT|REPO_FPR)="[0-9A-F]{40}"$', rf'\1="{fpr}"', text, count=1, flags=re.M)
    if n != 1:
        raise SystemExit(f"Failed to substitute ephemeral fingerprint in {p}")
    p.write_text(text)
PY
elif [[ "$FLOWS" != "source" ]]; then
    for f_check in \
        "$SITE/install/install.sh:REPO_KEY_FINGERPRINT" \
        "$SITE/install/pacman-repo.sh:REPO_FPR" \
        "$SITE/install/direct-pkg.sh:REPO_FPR"; do
        file="${f_check%%:*}"
        var="${f_check##*:}"
        actual="$(sed -n "s/^${var}=\"\\([^\"]*\\)\"\$/\\1/p" "$file")"
        [[ "$actual" == "$SIGN_FPR" ]] || {
            echo "Error: $file $var ($actual) does not match signed repo fingerprint ($SIGN_FPR)" >&2
            exit 1
        }
    done
fi

if [[ "$FLOWS" != "source" ]]; then
    cp -f "$WORK/super-desktop.gpg" "$SITE/repo/super-desktop.gpg"
    cp -a "$STAGED_REPO/." "$SITE/repo/$ARCH/"
    cp -f "$STAGED_ASSETS/$PKG_NAME" "$SITE/releases/download/$TAG/"
    cp -f "$STAGED_ASSETS/SHA256SUMS" "$SITE/releases/download/$TAG/"
    if [[ -f "$STAGED_ASSETS/$PKG_NAME.sig" ]]; then
        cp -f "$STAGED_ASSETS/$PKG_NAME.sig" "$SITE/releases/download/$TAG/"
    fi
    printf '{"tag_name": "%s"}\n' "$TAG" >"$SITE/api/releases/latest"
fi

if [[ "$FLOWS" != "pacman" ]]; then
    echo "==> Creating bare HTTPS git mirror for $TAG..."
    bare_git="$SITE/git/super-desktop.git"
    git init --bare -q "$bare_git"
    commit="$(git -C "$STAGE_SRC" rev-parse "refs/tags/$TAG^{commit}")"
    git -C "$bare_git" fetch --quiet "$STAGE_SRC" "refs/tags/$TAG:refs/tags/$TAG"
    git -C "$bare_git" update-ref refs/heads/master "$commit"
    git -C "$bare_git" symbolic-ref HEAD refs/heads/master
    git -C "$bare_git" repack -a -d -q
    git -C "$bare_git" update-server-info
fi

cat >"$FIXTURE/https_server.py" <<'PY'
import functools
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import ssl

class QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

handler = functools.partial(QuietHandler, directory="/fixture/site")
server = ThreadingHTTPServer(("127.0.0.1", 8443), handler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain("/fixture/tls/server.crt", "/fixture/tls/server.key")
server.socket = ctx.wrap_socket(server.socket, server_side=True)
server.serve_forever()
PY

cat >"$FIXTURE/run-as-omarchy.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

cp /fixture/tls/ca.crt /etc/ca-certificates/trust-source/anchors/sd-test-ca.crt
update-ca-trust

python3 /fixture/https_server.py >/tmp/https-server.log 2>&1 &
srv_pid=$!
trap 'kill "$srv_pid" 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do
    if curl --proto '=https' --tlsv1.2 -fsSI https://127.0.0.1:8443/install/install.sh >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
curl --proto '=https' --tlsv1.2 -fsSI https://127.0.0.1:8443/install/install.sh >/dev/null

exec runuser -u omarchy -- env \
    HOME=/home/omarchy \
    USER=omarchy \
    LOGNAME=omarchy \
    TERM=xterm-256color \
    SUPER_DESKTOP_PACMAN_REPO_URL=https://127.0.0.1:8443/repo \
    SUPER_DESKTOP_RELEASE_BASE_URL=https://127.0.0.1:8443/releases/download \
    SUPER_DESKTOP_RELEASE_LATEST_API=https://127.0.0.1:8443/api/releases/latest \
    SUPER_DESKTOP_INSTALL_SH_URL=https://127.0.0.1:8443/install/install.sh \
    SUPER_DESKTOP_REPO=https://127.0.0.1:8443/git/super-desktop.git \
    bash -euo pipefail -s -- "$@"
EOF
chmod 755 "$FIXTURE/run-as-omarchy.sh" "$FIXTURE/https_server.py"
chmod -R a+rX "$FIXTURE"

FRESH_IMG="sd-omarchy-fresh:${ARCH}"
echo "==> Building fresh Omarchy stable container $FRESH_IMG ($OCI_PLATFORM)..."
f_args=(build --platform "$OCI_PLATFORM" -f "$ROOT/packaging/arch/omarchy-fresh/Dockerfile" -t "$FRESH_IMG")
((NO_CACHE == 0)) || f_args+=(--no-cache)
f_args+=("$ROOT/packaging/arch")
"$ENGINE" "${f_args[@]}"

run_flow() {
    local name="$1"
    shift
    echo ""
    echo "================================================================"
    echo "==> Running Fresh Omarchy Flow: $name ($ARCH, $TAG)"
    echo "================================================================"
    "$ENGINE" run --rm -i \
        --platform "$OCI_PLATFORM" \
        -e "FLOW_VER=$VER" \
        -e "FLOW_TAG=$TAG" \
        -e "FLOW_ARCH=$ARCH" \
        -e "FLOW_FPR=$SIGN_FPR" \
        -v "$FIXTURE:/fixture:ro" \
        "$FRESH_IMG" \
        /fixture/run-as-omarchy.sh "$@"
}

if [[ "$FLOWS" == "all" || "$FLOWS" == "pacman" ]]; then
    run_flow "1. install.sh (Pacman repo + idempotency + refresh hook + pacman -Syu + Broadway)" <<'FLOW1'
cd "$HOME"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/install.sh | bash

test ! -d "$HOME/.cargo"
test ! -d "$HOME/.rustup"
pacman-key --list-keys "$FLOW_FPR" >/dev/null
grep -qxF "SigLevel = Required DatabaseRequired" /etc/pacman.d/super-desktop.conf
test "$(grep -cFx "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf)" -eq 1
inc_line="$(grep -nFx "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf | cut -d: -f1)"
omarchy_line="$(grep -n "^\[omarchy\]" /etc/pacman.conf | cut -d: -f1)"
test "$inc_line" -lt "$omarchy_line"

pacman -Qk super-desktop-bin
ver="$(/usr/bin/super-desktop --version)"
[[ "$ver" == *"$FLOW_VER"* ]]

for bin in /usr/lib/super-desktop/super-desktop /usr/lib/super-desktop/super-desktop-client /usr/lib/super-desktop/sd-qml-host; do
    test -x "$bin"
    if ldd "$bin" | grep -F "not found"; then
        echo "Missing shared library for $bin" >&2
        exit 1
    fi
done

grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'hl.layer_rule({ match = { namespace = "super-desktop" }, blur = true, no_anim = true, animation = "none" })' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"
test -x "$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"

# Idempotency check
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/install.sh | bash
test "$(grep -cFx "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf)" -eq 1
test "$(grep -cF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua")" -eq 1
test "$(grep -cF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua")" -eq 1
test "$(grep -cF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua")" -eq 1

# Omarchy pacman refresh hook + pacman -Syu
tmpl="/usr/share/omarchy/default/pacman/pacman-stable.conf"
if [[ "$FLOW_ARCH" == "aarch64" && -f /usr/share/omarchy/default/pacman/aarch64/pacman-stable.conf ]]; then
    tmpl="/usr/share/omarchy/default/pacman/aarch64/pacman-stable.conf"
fi
sudo cp "$tmpl" /etc/pacman.conf
sudo sed -i "s/^DownloadUser = alpm$/DisableSandbox/" /etc/pacman.conf
"$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"
inc_line="$(grep -nFx "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf | cut -d: -f1)"
omarchy_line="$(grep -n "^\[omarchy\]" /etc/pacman.conf | cut -d: -f1)"
test "$inc_line" -lt "$omarchy_line"
sudo pacman -Syu --noconfirm

# Headless Broadway runtime + sd-qml-host check
export XDG_RUNTIME_DIR=/tmp/sd-rt
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
/usr/bin/super-desktop --help >/dev/null
/usr/bin/super-desktop harnesses >/dev/null

gtk4-broadwayd :5 >/tmp/broadway.log 2>&1 &
bw_pid=$!
trap 'kill "$bw_pid" 2>/dev/null || true' EXIT
sleep 0.5
GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 /usr/bin/super-desktop daemon >/tmp/daemon.log 2>&1 &
for _ in $(seq 1 60); do
    if /usr/bin/super-desktop status | grep -qE "Visible|Hidden"; then
        break
    fi
    sleep 0.1
done
/usr/bin/super-desktop status | grep -q "Hidden"
/usr/bin/super-desktop toggle
/usr/bin/super-desktop status | grep -q "Visible"
/usr/bin/super-desktop reload-theme
/usr/bin/super-desktop toggle
/usr/bin/super-desktop status | grep -q "Hidden"
/usr/bin/super-desktop kill
sleep 0.4
/usr/bin/super-desktop status | grep -q "Daemon not running"
! pgrep -u "$(id -u)" -f '^([^ ]*/)?super-desktop daemon( |$)' >/dev/null 2>&1
timeout 2 env QT_QPA_PLATFORM=offscreen /usr/lib/super-desktop/sd-qml-host </dev/null >/tmp/qml.log 2>&1 || true
echo "✓ Flow 1 passed"
FLOW1

    run_flow "2. docs/install/pacman-repo.sh + pacman-hook.sh" <<'FLOW2'
cd "$HOME"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/pacman-repo.sh | bash

test ! -d "$HOME/.cargo"
pacman-key --list-keys "$FLOW_FPR" >/dev/null
grep -qxF "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf
pacman -Qk super-desktop-bin
[[ "$(/usr/bin/super-desktop --version)" == *"$FLOW_VER"* ]]
grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"
test -x "$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"

rm -f "$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/pacman-hook.sh | bash
test -x "$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"

tmpl="/usr/share/omarchy/default/pacman/pacman-stable.conf"
if [[ "$FLOW_ARCH" == "aarch64" && -f /usr/share/omarchy/default/pacman/aarch64/pacman-stable.conf ]]; then
    tmpl="/usr/share/omarchy/default/pacman/aarch64/pacman-stable.conf"
fi
sudo cp "$tmpl" /etc/pacman.conf
sudo sed -i "s/^DownloadUser = alpm$/DisableSandbox/" /etc/pacman.conf
"$HOME/.config/omarchy/hooks/pre-refresh-pacman.d/super-desktop"
grep -qxF "Include = /etc/pacman.d/super-desktop.conf" /etc/pacman.conf
sudo pacman -Syu --noconfirm
echo "✓ Flow 2 passed"
FLOW2

    run_flow "3. docs/install/direct-pkg.sh (latest + pinned VER + TMPDIR cleanup)" <<'FLOW3'
cd "$HOME"
export TMPDIR=/tmp/sd-direct-tmp
mkdir -p "$TMPDIR"

curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/direct-pkg.sh | bash
test -z "$(ls -A "$TMPDIR")"
pacman-key --list-keys "$FLOW_FPR" >/dev/null
pacman -Qk super-desktop-bin
[[ "$(/usr/bin/super-desktop --version)" == *"$FLOW_VER"* ]]
grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"

curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/direct-pkg.sh | VER="$FLOW_VER" bash
test -z "$(ls -A "$TMPDIR")"
echo "✓ Flow 3 passed"
FLOW3

    run_flow "4. install.sh pinned release (SUPER_DESKTOP_VERSION=$TAG)" <<'FLOW4'
cd "$HOME"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/install.sh | SUPER_DESKTOP_VERSION="$FLOW_TAG" bash

test ! -d "$HOME/.cargo"
pacman-key --list-keys "$FLOW_FPR" >/dev/null
pacman -Qk super-desktop-bin
[[ "$(/usr/bin/super-desktop --version)" == *"$FLOW_VER"* ]]
grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"
echo "✓ Flow 4 passed"
FLOW4
fi

if [[ "$FLOWS" == "all" || "$FLOWS" == "source" ]]; then
    run_flow "5. docs/install/source.sh (Rust source build + Broadway + migration to binary pkg)" <<'FLOW5'
cd "$HOME"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/source.sh | bash

src="$HOME/.local/share/super-desktop/source"
test -d "$HOME/.cargo"
test -d "$src/.git"
[[ "$(git -C "$src" config --get superdesktop.channel)" == "releases" ]]
[[ "$(git -C "$src" describe --tags --exact-match HEAD)" == "$FLOW_TAG" ]]
test -L "$HOME/.local/bin/super-desktop"
[[ "$("$HOME/.local/bin/super-desktop" --version)" == *"$FLOW_VER"* ]]
for bin in "$src/target/release/super-desktop" "$src/target/release/super-desktop-client" "$src/target/release/sd-qml-host"; do
    test -x "$bin"
    if ldd "$bin" | grep -F "not found"; then
        echo "Missing shared library for $bin" >&2
        exit 1
    fi
done
test -f "$HOME/.local/share/applications/super-desktop.desktop"
grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"

export XDG_RUNTIME_DIR=/tmp/sd-rt
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
gtk4-broadwayd :5 >/tmp/broadway.log 2>&1 &
bw_pid=$!
trap 'kill "$bw_pid" 2>/dev/null || true' EXIT
sleep 0.5
GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 "$HOME/.local/bin/super-desktop" daemon >/tmp/daemon.log 2>&1 &
for _ in $(seq 1 60); do
    if "$HOME/.local/bin/super-desktop" status | grep -qE "Visible|Hidden"; then
        break
    fi
    sleep 0.1
done
"$HOME/.local/bin/super-desktop" toggle
"$HOME/.local/bin/super-desktop" status | grep -q "Visible"
"$HOME/.local/bin/super-desktop" kill
sleep 0.4
"$HOME/.local/bin/super-desktop" status | grep -q "Daemon not running"

# Migrate source install to prebuilt super-desktop-bin package
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/install.sh | bash
pacman -Qk super-desktop-bin
[[ "$(readlink "$HOME/.local/bin/super-desktop")" == "/usr/bin/super-desktop" ]]
test -L "$HOME/.local/bin/super-desktop.before-package"
test ! -e "$HOME/.local/share/applications/super-desktop.desktop"
test -f "$HOME/.local/share/applications/super-desktop.desktop.before-package"
[[ "$(/usr/bin/super-desktop --version)" == *"$FLOW_VER"* ]]
echo "✓ Flow 5 passed"
FLOW5

    run_flow "6. docs/install/master.sh (branch source build)" <<'FLOW6'
cd "$HOME"
curl --proto '=https' --tlsv1.2 -fsSL https://127.0.0.1:8443/install/master.sh | bash

src="$HOME/.local/share/super-desktop/source"
test -d "$HOME/.cargo"
[[ "$(git -C "$src" symbolic-ref -q --short HEAD)" == "master" ]]
[[ "$(git -C "$src" config --get superdesktop.channel)" == "branch" ]]
[[ "$("$HOME/.local/bin/super-desktop" --version)" == *"$FLOW_VER"* ]]
for bin in "$src/target/release/super-desktop" "$src/target/release/super-desktop-client" "$src/target/release/sd-qml-host"; do
    test -x "$bin"
done
grep -qF -- "-- >>> super-desktop shortcut" "$HOME/.config/hypr/bindings.lua"
grep -qF 'o.exec_on_start("super-desktop daemon")' "$HOME/.config/hypr/autostart.lua"
grep -qF 'namespace = "super-desktop"' "$HOME/.config/hypr/hyprland.lua"
test -x "$HOME/.config/omarchy/hooks/theme-set.d/super-desktop"
echo "✓ Flow 6 passed"
FLOW6
fi

echo ""
echo "✓ All fresh Omarchy release verification flows passed ($ARCH, $TAG, flows=$FLOWS)."
