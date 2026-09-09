#!/usr/bin/env bash
set -euo pipefail

echo "=== Verifying crates/kobold-tool-fs ==="
cargo check -p kobold-tool-fs
cargo test -p kobold-tool-fs
cargo clippy -p kobold-tool-fs

echo "=== Verifying crates/kobold-tool-bash ==="
cargo check -p kobold-tool-bash
cargo test -p kobold-tool-bash
cargo clippy -p kobold-tool-bash

echo "=== Verifying full crates workspace test suite ==="
cargo test -p kobold-types -p kobold-context -p kobold-kernel -p kobold-tool-fs -p kobold-tool-bash

echo "=== All TASK-4 verifications passed successfully ==="
