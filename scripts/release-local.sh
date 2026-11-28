#!/usr/bin/env bash
# Build, test, package, GPG-sign, and optionally upload signed SUPER DESKTOP
# release packages locally using rootless Podman (or Docker) + host GPG/repo-add.
#
# Usage:
#   scripts/release-local.sh --tag vX.Y.Z [options]
#
# Options:
#   --tag vX.Y.Z       Existing annotated release tag to package (required)
#   --arch SPEC        x86_64, aarch64, x86_64,aarch64, or all (default: all)
#   --key-file PATH    GPG private key file (default: $SUPER_DESKTOP_SIGNING_KEY,
#                      ~/Documents/super-desktop-signing-keys/private-key.asc,
#                      or ~/.config/super-desktop-signing/private-key.asc)
#   --no-sign          Skip GPG signing and pacman-repo-*.tar.gz creation
#   --output DIR       Output directory (default: /tmp/super-desktop-release-<tag>)
#   --engine NAME      Container engine: podman or docker (default: auto-detect)
#   --no-cache         Pass --no-cache when building the container image
#   --fresh-flows SPEC Fresh Omarchy test flows: all, pacman, or source
#                      (default: all on native arch, pacman on cross-arch)
#   --skip-fresh-test  Skip end-to-end fresh Omarchy installer verification
#   --upload           Upload staged assets to GitHub Release <tag> and trigger
#                      pages.yml if both pacman-repo-*.tar.gz archives exist
#   -h, --help         Show this help text
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TAG=""
ARCH_SPEC="all"
KEY_FILE="${SUPER_DESKTOP_SIGNING_KEY:-}"
NO_SIGN=0
OUTPUT=""
ENGINE=""
NO_CACHE=0
FRESH_FLOWS=""
SKIP_FRESH_TEST=0
UPLOAD=0

usage() {
    sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
}

while (($# > 0)); do
    case "$1" in
    --tag)
        TAG="${2:?Missing value for --tag}"
        shift 2
        ;;
    --arch)
        ARCH_SPEC="${2:?Missing value for --arch}"
        shift 2
        ;;
    --key-file)
        KEY_FILE="${2:?Missing value for --key-file}"
        shift 2
        ;;
    --no-sign)
        NO_SIGN=1
        shift
        ;;
    --output)
        OUTPUT="${2:?Missing value for --output}"
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
    --fresh-flows)
        FRESH_FLOWS="${2:?Missing value for --fresh-flows}"
        shift 2
        ;;
    --skip-fresh-test)
        SKIP_FRESH_TEST=1
        shift
        ;;
    --upload)
        UPLOAD=1
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

[[ "$TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || {
    echo "Error: --tag vX.Y.Z is required" >&2
    exit 1
}
VER="${BASH_REMATCH[1]}"

if [[ "$(git -C "$ROOT" cat-file -t "refs/tags/$TAG" 2>/dev/null || true)" != "tag" ]]; then
    echo "Error: Refs/tags/$TAG does not exist or is not an annotated git tag" >&2
    exit 1
fi

ARCHES=()
declare -A _seen_arch=()
IFS=',' read -ra _raw_arches <<<"$ARCH_SPEC"
for token in "${_raw_arches[@]}"; do
    case "$token" in
    all)
        for a in x86_64 aarch64; do
            if [[ -z "${_seen_arch[$a]:-}" ]]; then
                ARCHES+=("$a")
                _seen_arch["$a"]=1
            fi
        done
        ;;
    x86_64 | aarch64)
        if [[ -z "${_seen_arch[$token]:-}" ]]; then
            ARCHES+=("$token")
            _seen_arch["$token"]=1
        fi
        ;;
    *)
        echo "Error: Unsupported --arch value '$token' (expected x86_64, aarch64, all, or comma-separated)" >&2
        exit 1
        ;;
    esac
done
((${#ARCHES[@]} > 0)) || {
    echo "Error: No target architectures selected" >&2
    exit 1
}

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

if [[ -z "$OUTPUT" ]]; then
    OUTPUT="/tmp/super-desktop-release-${TAG}"
fi
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"

for sub in release-assets arch repo; do
    if [[ -d "$OUTPUT/$sub" ]] && [[ -n "$(ls -A "$OUTPUT/$sub" 2>/dev/null || true)" ]]; then
        echo "Error: Output subdirectory already contains files: $OUTPUT/$sub" >&2
        echo "Use a clean --output directory before running again." >&2
        exit 1
    fi
done

HOST_ARCH="$(uname -m)"
for arch in "${ARCHES[@]}"; do
    if [[ "$arch" != "$HOST_ARCH" ]]; then
        binfmt_file="/proc/sys/fs/binfmt_misc/qemu-${arch}"
        if [[ ! -r "$binfmt_file" ]] || ! head -n1 "$binfmt_file" | grep -qx 'enabled'; then
            echo "Error: Cross-arch build for $arch on $HOST_ARCH requires enabled binfmt_misc handler ($binfmt_file)." >&2
            echo "Install QEMU user static handlers and restart systemd-binfmt:" >&2
            echo "  sudo pacman -S qemu-user-static qemu-user-static-binfmt" >&2
            echo "  sudo systemctl restart systemd-binfmt" >&2
            exit 1
        fi
    fi
done

if ((NO_SIGN == 0)); then
    for cmd in gpg repo-add tar sha256sum; do
        command -v "$cmd" >/dev/null 2>&1 || {
            echo "Error: Required host tool missing: $cmd (install gnupg pacman-contrib)" >&2
            exit 1
        }
    done
    if [[ -z "$KEY_FILE" ]]; then
        for candidate in \
            "$HOME/Documents/super-desktop-signing-keys/private-key.asc" \
            "${XDG_CONFIG_HOME:-$HOME/.config}/super-desktop-signing/private-key.asc"; do
            if [[ -r "$candidate" ]]; then
                KEY_FILE="$candidate"
                break
            fi
        done
    fi
    [[ -n "$KEY_FILE" && -r "$KEY_FILE" ]] || {
        echo "Error: Signing key file not found or unreadable." >&2
        echo "Pass --key-file PATH, set SUPER_DESKTOP_SIGNING_KEY, or pass --no-sign." >&2
        exit 1
    }
else
    for cmd in tar sha256sum; do
        command -v "$cmd" >/dev/null 2>&1 || {
            echo "Error: Required host tool missing: $cmd" >&2
            exit 1
        }
    done
fi

if ((UPLOAD == 1)); then
    command -v gh >/dev/null 2>&1 || {
        echo "Error: --upload requires GitHub CLI (gh)" >&2
        exit 1
    }
    gh auth status >/dev/null 2>&1 || {
        echo "Error: gh is not authenticated. Run 'gh auth login' first." >&2
        exit 1
    }
    is_bad="$(gh release view "$TAG" --json isDraft,isPrerelease --jq '.isDraft or .isPrerelease')"
    [[ "$is_bad" == "false" ]] || {
        echo "Error: GitHub Release $TAG must exist and must not be a draft or prerelease" >&2
        exit 1
    }
fi

WORK_TMP="$(mktemp -d)"
GNUPG_WORK=""
cleanup() {
    rm -rf "$WORK_TMP"
    if [[ -n "$GNUPG_WORK" && -d "$GNUPG_WORK" ]]; then
        rm -rf "$GNUPG_WORK"
    fi
}
trap cleanup EXIT

STAGE_SRC="$WORK_TMP/src"
echo "==> Exporting exact annotated tag $TAG..."
git clone --no-local --branch "$TAG" "file://$ROOT" "$STAGE_SRC" >/dev/null 2>&1
grep -qxF "version = \"$VER\"" "$STAGE_SRC/Cargo.toml" || {
    echo "Error: $TAG Cargo.toml version does not match $VER" >&2
    exit 1
}
# Ensure unprivileged 'builder' user inside rootless containers can read /src:ro.
chmod -R a+rX "$STAGE_SRC"

mkdir -p "$OUTPUT/release-assets" "$OUTPUT/arch" "$OUTPUT/repo"

oci_platform_for() {
    case "$1" in
    x86_64) echo "linux/amd64" ;;
    aarch64) echo "linux/arm64" ;;
    *) return 1 ;;
    esac
}

SOURCE_COPIED=0
for arch in "${ARCHES[@]}"; do
    oci_platform="$(oci_platform_for "$arch")"
    image_tag="sd-arch-builder:${arch}"
    arch_out="$OUTPUT/arch/$arch"
    mkdir -p "$arch_out"
    chmod 755 "$arch_out"

    echo "==> Building container image $image_tag ($oci_platform)..."
    build_args=(build --platform "$oci_platform" -t "$image_tag")
    if ((NO_CACHE == 1)); then
        build_args+=(--no-cache)
    fi
    build_args+=("$ROOT/packaging/arch/builder")
    "$ENGINE" "${build_args[@]}"

    echo "==> Building, testing, and packaging $TAG for $arch..."
    "$ENGINE" run --rm \
        --platform "$oci_platform" \
        -e "RELEASE_TAG=$TAG" \
        -v "$STAGE_SRC:/src:ro" \
        -v "$arch_out:/out" \
        "$image_tag" \
        bash -euo pipefail -c '
            # In rootless Podman, restoring UID 0 ensures /out remains owned by the host user.
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

    src_tar="$arch_out/super-desktop-${VER}-source.tar.gz"
    bin_tar="$arch_out/super-desktop-${VER}-linux-${arch}.tar.gz"
    pkg_file="$arch_out/super-desktop-bin-${VER}-1-${arch}.pkg.tar.zst"
    for f in "$src_tar" "$bin_tar" "$pkg_file"; do
        [[ -r "$f" ]] || {
            echo "Error: Expected build output missing: $f" >&2
            exit 1
        }
    done
    if ((SOURCE_COPIED == 0)); then
        cp -f "$src_tar" "$OUTPUT/release-assets/"
        SOURCE_COPIED=1
    fi
    cp -f "$bin_tar" "$OUTPUT/release-assets/"
    cp -f "$pkg_file" "$OUTPUT/release-assets/"
done

if ((NO_SIGN == 0)); then
    echo "==> Signing packages and building static Pacman repositories..."
    GNUPG_WORK="$(mktemp -d)"
    chmod 700 "$GNUPG_WORK"
    export GNUPGHOME="$GNUPG_WORK"
    gpg --batch --quiet --import "$KEY_FILE"
    FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ { print $10; exit }')"
    [[ "$FPR" =~ ^[0-9A-F]{40}$ ]] || {
        echo "Error: Imported GPG key has invalid fingerprint" >&2
        exit 1
    }
    FPR_FILE="$ROOT/packaging/arch/super-desktop.gpg.fingerprint"
    if [[ -f "$FPR_FILE" ]]; then
        expected="$(tr -d '[:space:]' <"$FPR_FILE")"
        if [[ "$FPR" != "$expected" ]]; then
            echo "Error: Imported key fingerprint $FPR does not match $expected" >&2
            exit 1
        fi
    fi

    for arch in "${ARCHES[@]}"; do
        pkg="$OUTPUT/release-assets/super-desktop-bin-${VER}-1-${arch}.pkg.tar.zst"
        bash "$ROOT/scripts/build-pacman-repo.sh" \
            --repo-name super-desktop \
            --key-fingerprint "$FPR" \
            --output "$OUTPUT/repo/$arch" \
            "$pkg"
        cp -f "$OUTPUT/repo/$arch/super-desktop-bin-${VER}-1-${arch}.pkg.tar.zst.sig" "$OUTPUT/release-assets/"
        tar -C "$OUTPUT/repo/$arch" --owner=0 --group=0 -czf "$OUTPUT/release-assets/pacman-repo-${arch}.tar.gz" .
    done
fi

echo "==> Writing SHA256SUMS..."
(
    cd "$OUTPUT/release-assets"
    sha256sum *.tar.gz *.pkg.tar.zst >SHA256SUMS
)

if ((SKIP_FRESH_TEST == 0)); then
    for arch in "${ARCHES[@]}"; do
        flows_for_arch="$FRESH_FLOWS"
        if [[ -z "$flows_for_arch" ]]; then
            if [[ "$arch" == "$HOST_ARCH" ]]; then
                flows_for_arch="all"
            else
                flows_for_arch="pacman"
            fi
        fi
        fresh_args=(
            --tag "$TAG"
            --arch "$arch"
            --release-assets "$OUTPUT/release-assets"
            --src-dir "$STAGE_SRC"
            --flows "$flows_for_arch"
            --engine "$ENGINE"
        )
        if ((NO_SIGN == 0)); then
            fresh_args+=(--repo-dir "$OUTPUT/repo/$arch")
        fi
        if ((NO_CACHE == 1)); then
            fresh_args+=(--no-cache)
        fi
        echo "==> Running fresh Omarchy installer verification ($arch, flows=$flows_for_arch)..."
        bash "$ROOT/tests/omarchy_fresh_release_test.sh" "${fresh_args[@]}"
    done
fi

if ((UPLOAD == 1)); then
    echo "==> Uploading release assets to $TAG..."
    existing_assets="$(gh release view "$TAG" --json assets --jq '.assets[].name')"
    upload_files=()
    for f in "$OUTPUT"/release-assets/*; do
        name="$(basename "$f")"
        if [[ "$name" == "SHA256SUMS" ]] && ((${#ARCHES[@]} < 2)); then
            echo "Warning: Skipping SHA256SUMS upload on single-arch run; upload SHA256SUMS with --arch all so both architectures are covered without collision." >&2
            continue
        fi
        if grep -qxF "$name" <<<"$existing_assets"; then
            echo "Error: Asset '$name' already exists on release $TAG (published assets are immutable; never --clobber)" >&2
            exit 1
        fi
        upload_files+=("$f")
    done

    if ((${#upload_files[@]} == 0)); then
        echo "Error: Nothing to upload" >&2
        exit 1
    fi

    gh release upload "$TAG" "${upload_files[@]}"

    after_assets="$(gh release view "$TAG" --json assets --jq '.assets[].name')"
    if grep -qxF "pacman-repo-x86_64.tar.gz" <<<"$after_assets" &&
        grep -qxF "pacman-repo-aarch64.tar.gz" <<<"$after_assets"; then
        echo "==> Triggering pages.yml on master to publish signed Pacman repositories..."
        gh workflow run pages.yml --ref master
    else
        echo "Note: Both pacman-repo-x86_64.tar.gz and pacman-repo-aarch64.tar.gz must exist on $TAG before pages.yml updates /repo."
    fi
fi

echo ""
echo "✓ Local release build complete: $OUTPUT/release-assets"
ls -1 "$OUTPUT/release-assets"
