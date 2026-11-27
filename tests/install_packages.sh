#!/usr/bin/env bash
# Exercise install.sh's package step with piped stdin and a private terminal.
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export work

# Replace only the final entry point. All package decisions come from install.sh.
sed '$c install_packages' install.sh > "$work/package-step"

pacman() {
    case "$1" in
        -T)
            [[ $scenario != query-failure ]] || return 1
            [[ $scenario != present ]] || return 0
            if [[ -f "$work/updated" ]]; then
                [[ $scenario != supplied-by-update ]] || return 0
                printf '%s\n' gst-plugins-good gst-libav
            else
                printf '%s\n' vte4 gst-plugins-good gst-libav
            fi
            return 127
            ;;
        -Sp)
            case "$scenario" in
                other-error) echo 'error: target not found' >&2; return 1 ;;
                compatible|install-failure) return 0 ;;
            esac
            if [[ ! -f "$work/updated" || $scenario == still-conflicted ]]; then
                echo ":: installing gstreamer (1.28.9-3) breaks dependency 'gstreamer=1.28.9-1' required by gst-plugins-bad-libs"
                echo 'error: failed to prepare transaction (could not satisfy dependencies)' >&2
                return 1
            fi
            ;;
        -Qdq) return 0 ;;
        *) return 2 ;;
    esac
}
sudo() {
    printf '%s\n' "$*" >> "$work/commands"
    [[ $1 == pacman && $2 == -S && $scenario != install-failure ]]
}
omarchy() {
    printf '%s\n' "omarchy $*" >> "$work/commands"
    [[ $1 == update && -t 0 && $scenario != update-failure ]] || return 1
    touch "$work/updated"
}
export -f pacman sudo omarchy

check() {
    export scenario="$1"
    local expected_status="$2" expected_commands="$3" status=0
    rm -f "$work/updated"
    : > "$work/commands"
    SHELL=/bin/bash script -qefc 'bash < "$work/package-step"' /dev/null \
        </dev/null > "$work/output" || status=$?
    if [[ "$status" != "$expected_status" || "$(cat "$work/commands")" != "$expected_commands" ]]; then
        cat "$work/output" "$work/commands" >&2
        echo "Unexpected package step result for $scenario (exit $status)" >&2
        exit 1
    fi
}

install="pacman -S --needed --noconfirm"
check present 0 ""
check compatible 0 "$install vte4 gst-plugins-good gst-libav"
check conflict 0 $'omarchy update\n'"$install gst-plugins-good gst-libav"
check supplied-by-update 0 "omarchy update"
check still-conflicted 1 "omarchy update"
grep -q 'gstreamer=1.28.9-1' "$work/output"
check update-failure 1 "omarchy update"
check other-error 1 ""
grep -q 'target not found' "$work/output"
check install-failure 1 "$install vte4 gst-plugins-good gst-libav"
check query-failure 1 ""
echo "Installer package checks passed."
