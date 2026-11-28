#!/usr/bin/env bash
# Short, repeatable run of every fuzz target from its committed seeds.
# Needs nightly cargo and cargo-fuzz: `just fuzz-smoke` and the flake check
# `fuzz-smoke` provide them.
#
# A crashing input is saved under fuzz/artifacts/.
#
# libFuzzer writes what it finds into the first corpus directory, so that one
# is a throwaway and the committed seeds are passed second, read-only.
set -euo pipefail

cd "$(dirname "$0")/../fuzz"
runs=${FUZZ_SMOKE_RUNS:-100000}
# libFuzzer reads a signed int and treats a negative count as unlimited.
if [[ ! $runs =~ ^0*([1-9][0-9]{0,9})$ ]] || (( BASH_REMATCH[1] > 2147483647 )); then
    echo "FUZZ_SMOKE_RUNS must be an integer from 1 to 2147483647" >&2
    exit 2
fi
runs=${BASH_REMATCH[1]}
work=$(mktemp -d -t skorupa-fuzz-smoke.XXXXXX)
trap 'rm -rf "${work:?}"' EXIT

# Listed first, as its own command: a failed or empty listing must fail the
# run, not skip it.
targets=$(cargo fuzz list --fuzz-dir .)
[ -n "$targets" ] || {
    echo "cargo fuzz list found no fuzz target" >&2
    exit 1
}

for target in $targets; do
    seeds="seeds/$target"
    [ -n "$(find "$seeds" -type f -print -quit 2>/dev/null)" ] || {
        echo "fuzz target $target has no seeds in fuzz/$seeds" >&2
        exit 1
    }
    mkdir "$work/$target"
    cargo fuzz run --fuzz-dir . "$target" "$work/$target" "$seeds" -- \
        -runs="$runs" -seed=1 -max_len=4096
done
echo "fuzz smoke: $(echo "$targets" | wc -w) target(s), $runs runs each"
