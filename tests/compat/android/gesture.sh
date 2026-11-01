#!/system/bin/sh
# Write touch gestures for compat scripts as input lines (protocol v2,
# host/bridge/src/input/proto.rs), paced in real time. The script runs inside the
# session, and its output is piped into the probes APK's
# dev.sargunv.innkeeper.probes.InputStdinHarness.
#   gesture.sh pinch|pan|doubletap X Y DISPLAY W H
# X Y are in the pixels of the app's W x H window on Android display DISPLAY.
g=$1 x=$2 y=$3 t=1000 d=$4 w=$5 h=$6
echo "window $w $h 1.000 $d"
# The harness (app_process) needs about a second to start reading. It clamps
# events that arrive in a burst to its clock, which squeezes a double tap's gap
# below GestureDetector's 40 ms minimum, so the script waits before the timed
# part.
sleep 1.5
case $g in
  pinch) # two fingers 80 px apart spreading to 560 px: zoom in
    i=0 s=40
    echo "touch_down $t 1 $((x - s)).0 $y.0 $d"
    echo "touch_down $t 2 $((x + s)).0 $y.0 $d"
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
    echo "touch_down $t 1 $cx.0 $y.0 $d"
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
      echo "touch_down $t 1 $x.0 $y.0 $d"
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
    echo "usage: gesture.sh pinch|pan|doubletap X Y DISPLAY W H" >&2
    exit 1
    ;;
esac
sleep 0.3
