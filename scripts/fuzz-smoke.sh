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
work=$(mktemp -d -t skorupa-fuzz-smoke.XXXXXX)
trap 'rm -rf "${work:?}"' EXIT

for target in $(cargo fuzz list --fuzz-dir .); do
    seeds="seeds/$target"
    [ -d "$seeds" ] || {
        echo "fuzz target $target has no seeds in fuzz/$seeds" >&2
        exit 1
    }
    mkdir "$work/$target"
    cargo fuzz run --fuzz-dir . "$target" "$work/$target" "$seeds" -- \
        -runs="$runs" -seed=1 -max_len=4096
done
