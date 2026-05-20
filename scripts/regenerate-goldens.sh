#!/usr/bin/env bash
# Regenerate hokusai-compat goldens from libmypaint.
#
# Idempotent. Re-runs setup-parity.sh (cheap when already provisioned),
# rebuilds the C wrapper on demand, then drives every fixture script
# under crates/hokusai-compat/fixtures/ through libmypaint and writes
# the resulting PNG beside each script.
#
# Optional first arg is a fixture-name substring filter, e.g.
#     ./scripts/regenerate-goldens.sh smudge
set -euo pipefail
cd "$(dirname "$0")/.."

./scripts/setup-parity.sh

echo
echo "==> regenerating goldens via libmypaint"
cargo xtask regenerate-goldens "$@"

echo
echo "==> running compat harness against the new goldens"
cargo test -p hokusai-compat

echo
echo "Done. Inspect any *.actual.png left under crates/hokusai-compat/fixtures/."
