# Developer commands. `just` lists them. Tools come from `nix develop`, or
# from the machine if it already has them.

nix := "nix --extra-experimental-features 'nix-command flakes'"

# One policy file for three lockfiles: an allowance that one workspace does
# not use (the Smithay git source, the Slint wrapper for chrono) is not worth
# a warning in each run. An unmaintained crate warns instead of failing; a
# vulnerability, an unsound crate and a yanked version fail.
deny_flags := "--warn unmaintained --allow unmatched-source --allow unused-wrapper"

[private]
default:
    @just --list --unsorted

# The merge gate. Checks formatting and changes no file.
check: (lint-test "Cargo.toml") (lint-test "compositor/Cargo.toml") lint-fuzz budget req-lint fuzz-smoke-config deny

# Clippy and the tests come from ONE compilation: Cargo builds the test
# targets with clippy-driver in place of rustc for workspace crates, and
# CLIPPY_ARGS is the channel `cargo clippy` itself uses to pass `-D warnings`.
# Running `cargo clippy` and `cargo test` separately compiles everything
# twice. `cargo ok` wraps exactly this where it is installed; the lines below
# it are the same steps spelled out, and the flake check does the same.
#
# Format check, Clippy with -D warnings, tests and doctests of one workspace.
[private]
lint-test manifest:
    #!/usr/bin/env bash
    set -euo pipefail
    if command -v cargo-ok >/dev/null; then
        exec cargo ok --check --locked --manifest-path {{ manifest }}
    fi
    cargo fmt --manifest-path {{ manifest }} --all -- --check
    RUSTC_WORKSPACE_WRAPPER=$(rustup which clippy-driver 2>/dev/null || command -v clippy-driver)
    export RUSTC_WORKSPACE_WRAPPER CLIPPY_ARGS=-D__CLIPPY_HACKERY__warnings__CLIPPY_HACKERY__
    tests="cargo test --locked --manifest-path {{ manifest }} --workspace --all-targets"
    $tests --no-run
    $tests
    cargo test --locked --manifest-path {{ manifest }} --workspace --doc

# It type-checks on stable; running a target needs nightly (`just fuzz-smoke`).
#
# The fuzz workspace has no tests, so linting it is its one compilation.
[private]
lint-fuzz:
    cargo fmt --manifest-path fuzz/Cargo.toml -- --check
    cargo clippy --locked --manifest-path fuzz/Cargo.toml --all-targets -- -D warnings

# Lines of code of every Rust file, per component, against budget.toml (needs tokei).
budget:
    python3 -B scripts/budget.py

# Every test names a requirement id; ids in tests and spec/*.qnt exist in docs/requirements.md.
req-lint:
    python3 -B scripts/req-lint.py

# Advisories, licenses, bans and sources for each lockfile. Fetches the RustSec database.
deny:
    cargo deny --locked --manifest-path Cargo.toml --config deny.toml check {{ deny_flags }}
    cargo deny --locked --manifest-path compositor/Cargo.toml --config deny.toml check {{ deny_flags }}
    cargo deny --locked --manifest-path fuzz/Cargo.toml --config deny.toml check {{ deny_flags }}

# Check the fuzz wrapper's run count without building a target.
[private]
fuzz-smoke-config:
    bash tests/scripts/fuzz-smoke.sh

# FUZZ_SMOKE_RUNS sets the number of inputs per target (default 100000).
#
# Short fixed-seed run of every fuzz target from its seeds, on the pinned nightly.
fuzz-smoke:
    {{ nix }} develop .#nightly --command scripts/fuzz-smoke.sh

# Advisories are the exception: they need the network, so only `just deny`
# covers them.
#
# The same checks in the Nix sandbox, plus the fuzz smoke run.
flake-check:
    {{ nix }} flake check --print-build-logs

# Everything: the gate, then the hermetic run.
check-all: check flake-check

# Set MERGE_GATE_CHANGE=1 when the branch changes the gate itself.
#
# Merge the pushed branch into main as one squash commit, after a green `just check` on that commit.
merge: hooks
    scripts/merge.sh

# Block direct pushes to main from this clone.
hooks:
    git config core.hooksPath .githooks
