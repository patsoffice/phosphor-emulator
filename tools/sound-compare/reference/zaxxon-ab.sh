#!/usr/bin/env sh
#
# Build listenable A/B wavs for Zaxxon's discrete sound board: each voice beside
# the reference emulator's recording of it, and beside its own previous
# revision.
#
#     ./tools/sound-compare/reference/zaxxon-ab.sh <sample-dir> <out-dir> [old-capture-dir]
#
# WHAT THE REFERENCE IS, AND IS NOT
#
# The reference emulator plays recorded WAVs for this board, so these files are
# a recording of one cabinet rather than a model of any board. They are useful
# for two things and no others: learning what a voice IS, and noticing that
# something is wrong. Two constants have been fitted to them in the past and
# both were reverted (BATTLESHIP_HZ to 750 Hz, and the alarm divider to 1QB,
# which is not wired to anything). Nothing in machines/src/zaxxon_sound.rs is
# fitted to a number measured here, and the transcription in
# docs/schematics/zaxxon-discrete-sound.md records four separate cases where
# these recordings turned out not to be of the circuit they are named after.
#
# The samples are not redistributable and are not in this repo. Get them from
# the MAME sample set (progettosnaps, samples/zip/zaxxon.zip) and unzip them
# into <sample-dir>. The voice-to-file mapping below is from
# src/mame/sega/zaxxon_a.cpp.
#
# THE WINDOWS ARE NOT DECORATION
#
# Each row carries the offset and duration to take from our capture so that it
# matches the length of the sample it is compared against. These voices run from
# 47 ms to 3.8 s, and a mismatched window moves the 125-250 Hz band by 5 dB on
# its own. The offsets also skip each scenario's pre-trigger lead-in, and for
# the engine they land inside one ladder step rather than across a glide.
#
# Both sides are peak-normalized, so what you hear is shape and not level. The
# board's real balance between voices is the eleven-leg mix table and is checked
# by `voice_levels_follow_the_leg_table`, not here.
set -eu

SAMPLES="${1:?usage: zaxxon-ab.sh <sample-dir> <out-dir> [old-capture-dir]}"
OUT="${2:?usage: zaxxon-ab.sh <sample-dir> <out-dir> [old-capture-dir]}"
OLD="${3:-}"

SNDCMP="${SNDCMP:-./target/debug/sndcmp}"
SOX="${SOX:-sox}"
command -v "$SOX" >/dev/null 2>&1 || SOX="$(command -v sox)"

mkdir -p "$OUT"

# voice  mame-file  trim-start  trim-length  repeats-for-listening
#
# `m-exp-sustained` rather than `m-exp` is deliberate and is the one row here
# that is not the obvious scenario. The game pulses M-EXP instead of striking it
# once, the 74123 is retriggerable, and 10.wav is accordingly flat for two
# seconds before it decays. Held against the single-trigger scenario our side is
# a thump where the reference is a roar, and the octave-band numbers do not show
# it at all, because retriggering changes the envelope and not the spectrum.
# That is worth knowing about the band metric as well as about the voice.
ROWS='
battleship 00 0.05 0.34 3
laser 01 0.05 0.20 4
base-missile 02 0.05 0.72 2
homing-missile 03 0.30 0.46 3
ship-engine-b 04 0.30 0.75 2
ship-engine-a 05 0.60 0.60 2
cannon 08 0.05 1.19 2
m-exp-sustained 10 0.05 3.83 1
s-exp 11 0.05 1.75 1
alarm3 20 0.06 0.047 6
alarm2 21 0.06 0.078 6
shot 23 0.05 0.99 2
'

echo "$ROWS" | while read -r v m s d n; do
  [ -n "${v:-}" ] || continue

  "$SNDCMP" capture "zaxxon/$v" --out "$OUT/.cap.wav" >/dev/null

  # 10.wav is the only sample at 22 kHz, so everything is forced to 44.1 kHz
  # rather than letting one file play back at a different rate.
  "$SOX" "$SAMPLES/$m.wav" -r 44100 -b 16 "$OUT/$v-mame.wav" \
      gain -n -3 repeat $((n - 1))
  "$SOX" "$OUT/.cap.wav" -r 44100 -b 16 "$OUT/$v-ours.wav" \
      trim "$s" "$d" gain -n -3 repeat $((n - 1))
  "$SOX" -n -r 44100 -b 16 -c 1 "$OUT/.gap.wav" trim 0 0.35
  "$SOX" "$OUT/$v-mame.wav" "$OUT/.gap.wav" "$OUT/$v-ours.wav" "$OUT/$v-AB.wav"

  # And against a previous revision's captures, if some were handed in. This is
  # the comparison the scenarios exist for: the samples cannot settle which pin
  # a wire is on, but a capture either side of a change can say whether the
  # change moved the voice and by how much.
  if [ -n "$OLD" ] && [ -f "$OLD/$v.wav" ]; then
    "$SOX" "$OLD/$v.wav" -r 44100 -b 16 "$OUT/$v-before.wav" \
        trim "$s" "$d" gain -n -3 repeat $((n - 1))
    cp "$OUT/$v-ours.wav" "$OUT/$v-after.wav"
    "$SOX" "$OUT/$v-before.wav" "$OUT/.gap.wav" "$OUT/$v-after.wav" \
        "$OUT/$v-BEFORE-AFTER.wav"
    # A voice a change was not supposed to touch should null out. Subtracting
    # the two and comparing against the new one is the audible equivalent of a
    # byte-identical golden frame, and it is worth as much: it is how you tell
    # "I changed four voices" from "I changed four voices and something else".
    # Halved going in, because two files already normalized to -3 dBFS will
    # clip a sample-by-sample difference when they disagree, and doubled again
    # in the ratio. A clipped difference would understate exactly the voices
    # that moved most.
    "$SOX" -m -v 0.5 "$OUT/$v-before.wav" -v -0.5 "$OUT/$v-after.wav" \
        "$OUT/.diff.wav"
    dr=$("$SOX" "$OUT/.diff.wav" -n stat 2>&1 | awk '/RMS[ ]+amp/{print $3}')
    ar=$("$SOX" "$OUT/$v-after.wav" -n stat 2>&1 | awk '/RMS[ ]+amp/{print $3}')
    rm -f "$OUT/.diff.wav"
    awk -v v="$v" -v d="$dr" -v a="$ar" 'BEGIN{
      d = 2 * d
      if (d + 0 <= 0) printf "%-16s unchanged\n", v
      else printf "%-16s difference %.1f dB below the new capture\n", v,
                  20 * log(a / d) / log(10)
    }'
  else
    echo "$v-AB.wav"
  fi

  rm -f "$OUT/.cap.wav" "$OUT/.gap.wav"
done
