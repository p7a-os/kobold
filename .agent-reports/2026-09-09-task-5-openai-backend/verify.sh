#!/usr/bin/env bash
set -euo pipefail

echo "=== Verifying crates/kobold-backend-openai ==="
cargo check -p kobold-backend-openai
cargo test -p kobold-backend-openai
cargo clippy -p kobold-backend-openai

echo "=== Verifying all 6 crates in workspace ==="
cargo test -p kobold-types -p kobold-context -p kobold-kernel -p kobold-tool-fs -p kobold-tool-bash -p kobold-backend-openai

echo "=== All TASK-5 verifications passed successfully ==="
