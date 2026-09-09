#!/usr/bin/env bash
set -euo pipefail

echo "=== Verifying crates/kobold-types ==="
cargo check -p kobold-types
cargo test -p kobold-types
cargo clippy -p kobold-types

echo "=== Checking crate dependencies for I/O leakage ==="
# Verify kobold-types does not depend on tokio, reqwest, hyper, or std::fs/process wrappers
if cargo tree -p kobold-types | grep -E "tokio|hyper|reqwest"; then
    echo "ERROR: Unwanted I/O dependencies found in kobold-types!"
    exit 1
fi

echo "=== All TASK-1 verifications passed successfully ==="
