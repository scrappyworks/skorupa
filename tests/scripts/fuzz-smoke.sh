#!/usr/bin/env bash
# REQ-PROTO-003: smoke checks exercise every target for a finite run count.
set -euo pipefail

cd "$(dirname "$0")/../.."

# The stub exercises the wrapper and seed discovery without compiling a target.
cargo() {
    case "$2" in
        list) echo decode ;;
        run) [[ " $* " == *" -runs=$expected "* ]] ;;
        *) return 1 ;;
    esac
}
export -f cargo

for invalid in -1 0 42garbage 2147483648 18446744073709551617; do
    # Accepting or forwarding any of these is a failure.
    export expected="$invalid"
    if FUZZ_SMOKE_RUNS="$invalid" scripts/fuzz-smoke.sh >/dev/null 2>&1; then
        echo "smoke accepted invalid run count $invalid" >&2
        exit 1
    fi
done
for valid in 1 100000 2147483647; do
    export expected="$valid"
    FUZZ_SMOKE_RUNS="$valid" scripts/fuzz-smoke.sh >/dev/null
done

export expected=2
FUZZ_SMOKE_RUNS=0002 scripts/fuzz-smoke.sh >/dev/null
export expected=100000
unset FUZZ_SMOKE_RUNS
scripts/fuzz-smoke.sh >/dev/null
