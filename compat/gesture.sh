#!/system/bin/sh
# Touch gestures for compat scripts, as a Linux.apk input stream (see
# docs/input.md) paced in real time. Runs inside the session and is piped into
# dev.linuxapk.system.input.InputBridgeMain --stdin.
#   gesture.sh pinch|pan|doubletap X Y [DISPLAY W H]
# With DISPLAY (a per-app window), the stream is protocol v2 for that display
# and a W x H window.
g=$1 x=$2 y=$3 t=1000 d=${4:-} w=${5:-1280} h=${6:-800}
sfx=${d:+ $d}
if [ -n "$d" ]; then
  echo "hello 2 $w $h"
  echo "window $w $h 1.000 $d"
else
  echo "hello 1 $w $h"
  echo "window $w $h 1.000"
fi
# The bridge (app_process) needs about a second to start reading. Events that
# reach it in a burst are clamped to its clock, which squeezes a double tap's
# gap below GestureDetector's 40 ms minimum, so wait before the timed part.
sleep "${GESTURE_DELAY:-1.5}"
case $g in
  pinch) # two fingers 80 px apart spreading to 560 px: zoom in
    i=0 s=40
    echo "touch_down $t 1 $((x - s)).0 $y.0$sfx"
    echo "touch_down $t 2 $((x + s)).0 $y.0$sfx"
    echo touch_frame
    while [ $i -lt 20 ]; do
      i=$((i + 1)) t=$((t + 16)) s=$((s + 12))
      sleep 0.016
      echo "touch_motion $t 1 $((x - s)).0 $y.0"
      echo "touch_motion $t 2 $((x + s)).0 $y.0"
      echo touch_frame
    done
    t=$((t + 16))
    echo "touch_up $t 1"
    echo "touch_up $t 2"
    echo touch_frame
    ;;
  pan) # one finger 300 px to the left
    i=0 cx=$x
    echo "touch_down $t 1 $cx.0 $y.0$sfx"
    echo touch_frame
    while [ $i -lt 20 ]; do
      i=$((i + 1)) t=$((t + 16)) cx=$((cx - 15))
      sleep 0.016
      echo "touch_motion $t 1 $cx.0 $y.0"
      echo touch_frame
    done
    t=$((t + 16))
    echo "touch_up $t 1"
    echo touch_frame
    ;;
  doubletap)
    for _ in 1 2; do
      echo "touch_down $t 1 $x.0 $y.0$sfx"
      echo touch_frame
      sleep 0.05
      t=$((t + 50))
      echo "touch_up $t 1"
      echo touch_frame
      sleep 0.1
      t=$((t + 100))
    done
    ;;
  *)
    echo "usage: gesture.sh pinch|pan|doubletap X Y" >&2
    exit 1
    ;;
esac
sleep 0.3
