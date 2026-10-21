# Unified build tooling for all five Delego contract crates.
#
# The same recipe names are mirrored in each crate's package.json
# (build-wasm / test / lint) so contributors can use either entry point.
#
# Requires: cargo + the wasm32-unknown-unknown target (see README).
# Optional: soroban CLI (https://github.com/stellar/soroban-tools) for
# the optimize step — `build-all` skips optimization when soroban is absent.

# Names of the five contract crates (must match workspace member directory names).
contracts := "escrow permissions marketplace reputation delegation_registry"

# Canonical output directory for workspace release WASM artefacts.
wasm_dir := "target/wasm32-unknown-unknown/release"

build-wasm:
    cargo build --target wasm32-unknown-unknown --release --workspace --exclude tests

# Build every contract WASM, then strip debug symbols with `soroban contract
# optimize` where the CLI is available.  The optimized files are written
# alongside the originals with an `.optimized.wasm` suffix.
build-all: build-wasm
    #!/usr/bin/env sh
    set -e
    echo "==> Verifying WASM artefacts in {{wasm_dir}}"
    count=0
    for crate in {{contracts}}; do
        # Cargo uses hyphens->underscores in output file names.
        artifact="{{wasm_dir}}/$(echo "$crate" | tr '-' '_').wasm"
        if [ -f "$artifact" ]; then
            echo "  ✓ $artifact"
            count=$((count + 1))
        else
            echo "  ✗ MISSING: $artifact" >&2
            exit 1
        fi
    done
    echo "==> $count/5 contract WASM files present"
    if command -v soroban > /dev/null 2>&1; then
        echo "==> Optimizing with soroban contract optimize"
        for crate in {{contracts}}; do
            artifact="{{wasm_dir}}/$(echo "$crate" | tr '-' '_').wasm"
            soroban contract optimize --wasm "$artifact"
            echo "  ✓ optimized: $artifact"
        done
    else
        echo "==> soroban CLI not found — skipping optimize step (install from https://github.com/stellar/soroban-tools)"
    fi

test:
    cargo test --workspace

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

check: lint fmt-check
    cargo check --workspace

# Measure optimized WASM binary sizes, enforce the 64 KiB hard limit, and
# show a diff against the committed baseline.  Requires jq and either
# stellar-cli or soroban CLI to be installed for the optimize step.
# Pass --write-baseline to update scripts/wasm-size-baseline.json in-place.
benchmark *FLAGS: build-all
    bash scripts/wasm-benchmark.sh --no-build {{FLAGS}}

# Run every CI check locally in the same order as the GitHub Actions
# workflow.  A single `just ci` should catch every failure before pushing.
ci: fmt-check lint test build-all benchmark
    echo "✅ all CI checks passed"
