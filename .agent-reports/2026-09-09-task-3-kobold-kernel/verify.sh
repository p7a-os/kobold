#!/usr/bin/env bash
set -euo pipefail

echo "=== Verifying crates/kobold-kernel ==="
cargo check -p kobold-kernel
cargo test -p kobold-kernel
cargo clippy -p kobold-kernel

echo "=== Checking crate dependencies for network/socket leakage ==="
# Verify kobold-kernel does not pull hyper, reqwest, or fastwebsockets in production dependencies
if cargo tree -p kobold-kernel -e=no-dev | grep -E "hyper|reqwest|fastwebsockets"; then
    echo "ERROR: Network or websocket dependencies found in kobold-kernel!"
    exit 1
fi

echo "=== All TASK-3 verifications passed successfully ==="
