#!/bin/bash
# Regression capture of the GUI, headless (never touches a real screen): empty window -> open project
# -> timeline -> each dock tab -> clip sections -> menus -> settings -> palette -> export tab, at
# 1920x1080 and again at 1280x720. Writes numbered PNGs to OUT; look at them (or diff two runs).
#
#   scripts/ui_capture.sh OUT_DIR [PROJECT.edith]
#
# Env: DRIVE=<path to drive.sh>  (the headless sway + virtual pointer driver; its header says how
#                                 to install it)    BIN=<release edith binary, default target/release/edith>
#      SCRATCH=<dir for the driver's state, default $OUT/.state>
set -u
OUT=${1:?usage: ui_capture.sh OUT_DIR [PROJECT.edith]}
PROJECT=${2:-}
DRIVE=${DRIVE:?set DRIVE to drive.sh}
HERE=$(cd "$(dirname "$0")/.." && pwd)
BIN=${BIN:-$HERE/target/release/edith}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
export D=${SCRATCH:-$OUT/.state} BIN CWD=$HERE
mkdir -p "$D/home"

shot() { "$DRIVE" shot "$1" >/dev/null; mv "$D/$1.png" "$OUT/$1.png"; }
click() { "$DRIVE" click "$@"; }
key() { "$DRIVE" key "$@"; }

run() { # run WIDTH HEIGHT TAG
  local W=$1 H=$2 T=$3
  "$DRIVE" up "${W}x${H}" >/dev/null
  # empty window
  "$DRIVE" start; shot "${T}_01_empty"
  "$DRIVE" stop; sleep 1
  # the project (or stay empty when none was given)
  if [ -n "$PROJECT" ]; then "$DRIVE" start "$PROJECT"; else "$DRIVE" start; fi
  sleep 2; shot "${T}_02_project"
  local TAB_Y=72 MEDIA=$((W - 282)) CLIP=$((W - 201)) TRANS=$((W - 122)) EXPORT=$((W - 42))
  # dock tabs
  click $MEDIA $TAB_Y;  shot "${T}_03_dock_media"
  click $CLIP $TAB_Y;   shot "${T}_04_dock_clip_empty"
  click 400 $((H * 68 / 100)); sleep 0.5
  click $CLIP $TAB_Y;   shot "${T}_05_dock_clip_selected"
  click $((W - 230)) 154; shot "${T}_06_clip_section_open"
  click $TRANS $TAB_Y;  shot "${T}_07_dock_transcript"
  click $EXPORT $TAB_Y; shot "${T}_08_dock_export"
  click $MEDIA $TAB_Y
  # menus: File Edit Clip Timeline View Help
  local i=0
  for X in 56 99 143 202 263 314; do
    i=$((i + 1)); click $X 40; shot "${T}_09_menu_$i"; key -k Escape; sleep 0.4
  done
  # settings, then the palette
  key -M ctrl , -m ctrl; shot "${T}_10_settings"; key -k Escape; sleep 0.4
  key -M ctrl k -m ctrl; sleep 0.4; key "split"; shot "${T}_11_palette"; key -k Escape; sleep 0.4
  # the timeline after all that: nothing should have moved
  shot "${T}_12_final"
  "$DRIVE" down
}

run 1920 1080 w1080
run 1280 720 w720
echo "captures in $OUT"
