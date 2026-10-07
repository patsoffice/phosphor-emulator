#!/usr/bin/env sh
#
# Run the workspace's tests in two lanes, faster than `cargo test` without
# changing what any test sees.
#
#     ./scripts/test.sh                      # the workspace, minus CPU validation
#     ./scripts/test.sh -p phosphor-harness  # any `cargo test` selection instead
#     PHOSPHOR_ROMS=~/ws/mame-runtime/roms ./scripts/test.sh
#
# Arguments go to `cargo test` as they are, and replace the default
# `--workspace --exclude phosphor-cpu-validation` rather than adding to it.
# Run it from the dev shell (`nix develop`), which provides jq.
#
# Why two lanes. `cargo test` runs its test binaries one at a time, and almost
# all of a full run is seven ROM-gated binaries. Three of them are CPU-heavy:
# alone, audio_sanity_test, golden_frame_test and movie_test each keep 13 to 14
# of 16 cores busy for about 45 seconds, because they sweep the registry one
# machine per core. The other four slow ones (boot_check_test and the script
# crate's watchpoints, state_and_dip and frameshot_parity) take 10 to 16
# seconds each on one or two cores, and under `cargo test` they wait their turn
# while the rest of the machine idles. So the heavy three run one after another
# in one lane, each with the machine to itself, and every other binary runs in
# a parallel pool in the second lane beside them.
#
# Why not cargo-nextest, which can express the same split with test groups:
# it runs every test in its own process, and audio_sanity_test computes its
# sweep once in a OnceLock and shares it between three tests. Under nextest
# the sweep ran three times, and a full run was no faster than `cargo test`.
# This keeps each binary in one process, exactly as `cargo test` runs it.
#
# Each binary runs from its package directory with CARGO_MANIFEST_DIR set,
# which is what `cargo test` does. Its output is kept, and printed only if it
# fails. Doc tests run in the second lane after its pool drains.
#
# PHOSPHOR_TEST_HEAVY overrides the heavy list (space separated binary names),
# and PHOSPHOR_TEST_JOBS the pool size (default: the number of cores).

set -eu

cd "$(dirname "$0")/.."

[ "$#" -gt 0 ] || set -- --workspace --exclude phosphor-cpu-validation

HEAVY="${PHOSPHOR_TEST_HEAVY:-audio_sanity_test golden_frame_test movie_test}"
JOBS="${PHOSPHOR_TEST_JOBS:-$(getconf _NPROCESSORS_ONLN)}"

command -v jq >/dev/null 2>&1 || {
  echo "test.sh: jq not found; run from the dev shell (nix develop)" >&2
  exit 2
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM
mkdir "$work/log" "$work/status"

start="$(date +%s)"

# Build every test binary once, and list them: name, executable, package dir.
# `json-render-diagnostics` keeps compiler errors human-readable on stderr while
# stdout carries the JSON this reads.
cargo test --no-run --message-format=json-render-diagnostics "$@" > "$work/build.json"
jq -r '
  select(.reason == "compiler-artifact" and .executable != null and .profile.test)
  | [.target.name, .executable, (.manifest_path | sub("/Cargo.toml$"; ""))]
  | @tsv
' "$work/build.json" > "$work/all.tsv"

: > "$work/heavy.tsv"
: > "$work/rest.tsv"
while IFS="$(printf '\t')" read -r name exe dir; do
  case " $HEAVY " in
    *" $name "*) printf '%s\t%s\t%s\n' "$name" "$exe" "$dir" >> "$work/heavy.tsv" ;;
    *) printf '%s\t%s\t%s\n' "$name" "$exe" "$dir" >> "$work/rest.tsv" ;;
  esac
done < "$work/all.tsv"

built="$(date +%s)"
echo "built $(wc -l < "$work/all.tsv" | tr -d ' ') test binaries in $((built - start))s;" \
  "$(wc -l < "$work/heavy.tsv" | tr -d ' ') serial, the rest $JOBS at a time" >&2

# Run one binary from its package directory, record its exit status and time,
# and say how it went in one line. `$1` is the work dir, then name, exe, dir.
run_one='
  work=$1 name=$2 exe=$3 dir=$4
  id=$(basename "$exe")
  t0=$(date +%s)
  if (cd "$dir" && CARGO_MANIFEST_DIR="$dir" "$exe" > "$work/log/$id" 2>&1); then
    status=0
  else
    status=$?
  fi
  t1=$(date +%s)
  echo "$status" > "$work/status/$id"
  if [ "$status" -eq 0 ]; then word=ok; else word=FAIL; fi
  printf "%-4s %4ss  %s (%s)\n" "$word" "$((t1 - t0))" "$name" "$(basename "$dir")" >&2
'

# Lane 1: the heavy binaries, one at a time.
(
  while IFS="$(printf '\t')" read -r name exe dir; do
    sh -c "$run_one" sh "$work" "$name" "$exe" "$dir"
  done < "$work/heavy.tsv"
) &
heavy_pid=$!

# Lane 2: everything else in a pool, then the doc tests. An empty pool is not
# handed to xargs at all, because GNU xargs runs its command once on empty
# input.
if [ -s "$work/rest.tsv" ]; then
  tr '\t' '\n' < "$work/rest.tsv" | xargs -n 3 -P "$JOBS" sh -c "$run_one" sh "$work"
fi

# Doc tests belong to the selection only when it names no targets: cargo
# refuses `--doc` beside `--test`, `--lib` and the rest, and a run narrowed to
# one test binary did not ask for them.
want_doc=yes
for arg in "$@"; do
  case "$arg" in
    --lib | --bin | --bin=* | --bins | --test | --test=* | --tests | --example | --example=* \
      | --examples | --bench | --bench=* | --benches | --all-targets | --doc)
      want_doc=no ;;
  esac
done
doc_status=0
if [ "$want_doc" = yes ]; then
  cargo test --doc "$@" > "$work/log/doctests" 2>&1 || doc_status=$?
  if [ "$doc_status" -eq 0 ]; then
    echo "ok         doc tests" >&2
  else
    echo "FAIL       doc tests" >&2
  fi
fi

wait "$heavy_pid"
done_at="$(date +%s)"

# Report. A failure prints its whole log; a ROM-gated skip is only named, so a
# run without ROMs does not read as a run that checked them.
failed=0
for f in "$work/status/"*; do
  [ -e "$f" ] || continue
  if [ "$(cat "$f")" -ne 0 ]; then
    failed=$((failed + 1))
    id="$(basename "$f")"
    echo "" >&2
    echo "==== $id ====" >&2
    cat "$work/log/$id" >&2
  fi
done
if [ "$doc_status" -ne 0 ]; then
  failed=$((failed + 1))
  echo "" >&2
  echo "==== doc tests ====" >&2
  cat "$work/log/doctests" >&2
fi

skipped="$(grep -l 'skipping' "$work/log/"* 2>/dev/null | sed 's|.*/||' || true)"
if [ -n "$skipped" ]; then
  echo "" >&2
  echo "printed a skip (ROM-gated tests without their ROMs?):" >&2
  echo "$skipped" | sed 's/^/  /' >&2
fi

echo "" >&2
echo "tests ran in $((done_at - built))s, $((done_at - start))s with the build;" \
  "$failed failed" >&2
[ "$failed" -eq 0 ]
