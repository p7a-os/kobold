#!/usr/bin/env bash
set -euo pipefail

echo "=== 1. Cargo Check: kobold-runtime ==="
cargo check -p kobold-runtime

echo "=== 2. Clippy: kobold-runtime ==="
cargo clippy -p kobold-runtime -- -D warnings

echo "=== 3. Unit & Integration Tests: kobold-runtime ==="
cargo test -p kobold-runtime

echo "=== 4. Full Thin Harness Crate Suite ==="
cargo test -p kobold-types -p kobold-context -p kobold-kernel -p kobold-tool-fs -p kobold-tool-bash -p kobold-backend-openai -p kobold-runtime

echo "=== Verification Succeeded ==="
