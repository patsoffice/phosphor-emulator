#!/usr/bin/env bash
#
# Prove a MAME reference driver responds to its own timeline before any capture
# from it is trusted.
#
# This exists because every driver here once read the clock as the attotime's
# INTEGER seconds field, so a 50 ms pulse was really a one-second assertion and
# the release edge never happened. The captures looked entirely reasonable —
# plausible notes, of plausible length, starting when they should — and hours
# went into reconciling a model against a control voltage the board never holds.
#
# What caught it was not analysis. It was capturing twice with different hold
# lengths, finding the results byte-identical, then capturing with the effect
# never triggered and finding silence. Both are cheap; neither was being run.
#
# Two checks, and the driver must honour SND_VERIFY for them to mean anything:
#
#   null   — nothing is ever asserted. The capture must be near-silent.
#   nudge  — the schedule shifts 30 ms. The capture must CHANGE.
#
# The shift is sub-second on purpose. The bug this guards against quantised to
# whole seconds, so a check that moved an event by a second would have passed
# while the defect sat underneath it.
#
# Usage:
#   verify-reference.sh <driver.lua> <machine> [extra mame args...]
#
# e.g.
#   DK_EFFECT=jump verify-reference.sh drive_dkong_single.lua dkong -seconds_to_run 6
#
set -uo pipefail

if [ $# -lt 2 ]; then
  sed -n '3,30p' "$0" | sed 's/^# \?//'
  exit 2
fi

DRIVER=$1; shift
MACHINE=$1; shift

MAME=${MAME:-mame}
ROMS=${ROMS:-$HOME/ws/mame-runtime/roms}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

run() { # $1 = SND_VERIFY value ("" for the real schedule), $2 = output wav,
        # the rest are passed through to MAME
  local mode=$1; shift
  local out=$1; shift
  SND_VERIFY=$mode "$MAME" "$MACHINE" \
    -rompath "$ROMS" -nothrottle -video none -sound none \
    -cfg_directory "$WORK/cfg" -snapshot_directory "$WORK/snap" \
    -autoboot_script "$DRIVER" -wavwrite "$out" "$@" >"$WORK/log" 2>&1 \
    || { echo "  mame failed for mode '${mode:-real}':"; sed -n '1,5p' "$WORK/log"; exit 1; }
}

echo "verifying $DRIVER on $MACHINE"

run ""      "$WORK/base.wav"  "$@"
run "null"  "$WORK/null.wav"  "$@"
run "nudge" "$WORK/nudge.wav" "$@"

fail=0

# --- null: the capture must be quiet without the stimulus --------------------
# Compared by file size against silence is not enough — a WAV of digital silence
# is the same size as a loud one. Use the peak instead, via a tiny reader so this
# script needs nothing installed.
#
# Measured from SKIP_S onward, not over the whole file. These boards make
# power-on noise the moment they are switched on — Donkey Kong's peaks louder
# than any of its effects — which is why every driver here carries a pre-roll
# before it triggers anything. Measuring the whole file measures that noise, and
# the null check then "fails" identically for every mode, which is exactly what
# this script did on its first run.
SKIP_S=${SND_SKIP_S:-2.0}

peak() {
  python3 - "$1" "$SKIP_S" <<'PY'
import sys, wave, struct
w = wave.open(sys.argv[1], 'rb')
sr = w.getframerate()
d = struct.unpack('<%dh' % w.getnframes(), w.readframes(w.getnframes()))
d = d[int(float(sys.argv[2]) * sr):]
print(max(abs(x) for x in d) / 32767.0 if d else 0.0)
PY
}

base_peak=$(peak "$WORK/base.wav")
null_peak=$(peak "$WORK/null.wav")
# 0.01 of full scale is far below any effect worth capturing and above the
# board's idle noise floor.
if awk "BEGIN{exit !($null_peak < 0.01)}"; then
  echo "  ok   null         peak $null_peak with nothing asserted (driven: $base_peak)"
else
  echo "  FAIL null         peak $null_peak with NOTHING asserted, against $base_peak driven."
  echo "                    The capture contains something this driver does not drive."
  fail=1
fi

# --- nudge: the capture must change when the schedule moves ------------------
if cmp -s "$WORK/base.wav" "$WORK/nudge.wav"; then
  echo "  FAIL sensitivity  IDENTICAL capture with the schedule moved 30 ms."
  echo "                    The driver is not honouring its own timeline."
  fail=1
else
  echo "  ok   sensitivity  capture changes when the schedule moves 30 ms"
fi

if [ $fail -ne 0 ]; then
  echo
  echo "$DRIVER does not drive what it claims. Any measurement taken against it"
  echo "is unverified — do not compare against captures from this driver until"
  echo "it passes."
  exit 1
fi
