#!/usr/bin/env bash
set -euo pipefail

echo "=== Verifying crates/kobold-context ==="
cargo check -p kobold-context
cargo test -p kobold-context
cargo clippy -p kobold-context

echo "=== Checking crate dependencies for I/O leakage ==="
# Verify kobold-context does not depend on tokio, reqwest, or hyper in production dependencies
if cargo tree -p kobold-context -e=no-dev | grep -E "tokio|hyper|reqwest"; then
    echo "ERROR: Unwanted I/O dependencies found in kobold-context!"
    exit 1
fi

echo "=== All TASK-2 verifications passed successfully ==="
