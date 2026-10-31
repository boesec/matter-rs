#!/usr/bin/env bash
# Local quality gate. Runs the same checks as the CI workflow (.github/workflows/ci.yml):
# format check, clippy with warnings as errors, tests and cargo-deny.
#
# Usage: ./scripts/check.sh
# Every step runs even if an earlier one fails; the script exits with 1 if any step failed.

set -uo pipefail

cd "$(dirname "$0")/.."

# Prefer cargo from PATH; fall back to the rustup default location for shells started before
# the toolchain was installed.
if command -v cargo >/dev/null 2>&1; then
    cargo=cargo
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    cargo="$HOME/.cargo/bin/cargo"
else
    echo "cargo not found: install Rust through rustup (https://rustup.rs)." >&2
    exit 1
fi

failed=()

run() {
    local name=$1
    shift
    echo
    echo "==> cargo $*"
    if ! "$cargo" "$@"; then
        echo "FAILED: $name"
        failed+=("$name")
    fi
}

run fmt fmt --all --check
run clippy clippy --workspace --all-targets --all-features -- -D warnings
run test test --workspace
run deny deny check

echo
if [ ${#failed[@]} -gt 0 ]; then
    echo "Quality gate failed: ${failed[*]}"
    exit 1
fi
echo "Quality gate passed."
