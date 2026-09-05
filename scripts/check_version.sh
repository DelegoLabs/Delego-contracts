#!/usr/bin/env bash
set -e

echo "Running version consistency check for delego-marketplace..."

# 1. Extract version from Cargo.toml to ensure we can parse it
TOML_VERSION=$(grep -m1 '^version =' marketplace/Cargo.toml | sed -E 's/version = "(.*)"/\1/')
if [ -z "$TOML_VERSION" ]; then
    echo "Error: Could not extract version from marketplace/Cargo.toml"
    exit 1
fi

echo "Found marketplace version: $TOML_VERSION"

# 2. Run the dedicated unit test that asserts version() matches Cargo.toml dynamically
echo "Running version validation test..."
cargo test --package delego-marketplace --tests test_version_matches_cargo_toml -- --exact || {
    echo "Error: Version consistency test failed. The contract version() might have drifted from Cargo.toml."
    exit 1
}

echo "Version check passed successfully!"
