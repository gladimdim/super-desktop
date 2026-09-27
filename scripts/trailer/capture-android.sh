#!/usr/bin/env bash
# The phone take: harness list -> a live terminal -> the DUO split showing two
# PCs' agents side by side.
#
# Coordinates are for a Galaxy Z Fold 7 on its unfolded inner display
# (1968x2184). For another device, screenshot it and re-measure:
#   adb shell screencap -p /sdcard/s.png && adb pull /sdcard/s.png
#
# screenrecord pads the capture to 1968x2520 with black bars; build.sh crops
# them back off (crop=1968:2184:0:168).
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

OUT=${1:-$FOOTAGE/android-raw.mp4}
PKG=${PKG:-com.omarchy.ailauncher}
mkdir -p "$(dirname "$OUT")"

TAP1_X=425;  TAP1_Y=1220   # open a harness from the list
TAP2_X=1758; TAP2_Y=161    # the split-view control
TAP3_X=1244; TAP3_Y=1339   # the second harness, on the other PC

# SystemUI demo mode: a clean status bar, no personal notifications. Restored
# at the end whether or not the recording succeeds.
restore_status_bar() {
  adb shell am broadcast -a com.android.systemui.demo -e command exit >/dev/null 2>&1 || true
  adb shell settings put global sysui_demo_allowed 0 >/dev/null 2>&1 || true
}
trap restore_status_bar EXIT

adb shell settings put global sysui_demo_allowed 1 >/dev/null 2>&1 || true
for c in "command enter" \
         "command clock -e hhmm 1000" \
         "command battery -e level 100 -e plugged false" \
         "command network -e wifi show -e level 4" \
         "command network -e mobile hide" \
         "command notifications -e visible false"; do
  adb shell am broadcast -a com.android.systemui.demo -e $c >/dev/null 2>&1 || true
done

adb shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
adb shell sleep 3

adb shell "screenrecord --time-limit 12 --bit-rate 16000000 /sdcard/take.mp4 &
sleep 1.6
input tap $TAP1_X $TAP1_Y
sleep 2.8
input tap $TAP2_X $TAP2_Y
sleep 2.0
input tap $TAP3_X $TAP3_Y
sleep 4.5
wait"

adb pull -a /sdcard/take.mp4 "$OUT" >/dev/null
adb shell rm -f /sdcard/take.mp4
echo "$OUT"
