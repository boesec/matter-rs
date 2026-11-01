#!/usr/bin/env bash
# Local interop scenario: the On/Off node example commissioned and driven by the matter.js shell.
#
# Builds the example, starts it with a fresh state directory, commissions it with the shell
# through the pairing code the example prints, reads, toggles and writes through the shell,
# restarts the node on its persisted state and checks that it is still commissioned. The
# assertions are in the step files next to this script; the driver is drive-shell.mjs.
#
# Usage: ./tests/interop-local/matterjs-shell.sh <matter.js clone> [interface] [work directory]
#
# Needs: cargo, Node.js, a built matter.js clone (packages/nodejs-shell/dist/esm/app.js). The
# node listens on UDP ports 5540 and 5353. The logs land in the work directory and contain the
# pairing code; nothing of this scenario is committed except the scripts.
#
# Exit code 0 when every step passed, 1 otherwise.

set -uo pipefail

matterjs=${1:?usage: matterjs-shell.sh <matter.js clone> [interface] [work directory]}
interface=${2:-}
workdir=${3:-${TMPDIR:-/tmp}/matter-rs-interop-local}
port=${MATTER_RS_PORT:-5540}

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
app="$matterjs/packages/nodejs-shell/dist/esm/app.js"
if [ ! -f "$app" ]; then
    echo "matter.js shell not found at $app; build the clone first (see README.md)"
    exit 1
fi

if command -v cargo >/dev/null 2>&1; then cargo=cargo; else cargo="$HOME/.cargo/bin/cargo"; fi

echo '==> building the example'
"$cargo" build -p matter-rs-engine-rsmatter --example onoff_node --manifest-path "$root/Cargo.toml" || exit 1
example="${CARGO_TARGET_DIR:-$root/target}/debug/examples/onoff_node"

rm -rf "$workdir"
mkdir -p "$workdir"
state_dir="$workdir/node-state"
shell_storage="$workdir/controller-storage"
shell_log="$workdir/shell.log"

node_pid=""

start_node() {
    local name=$1
    shift
    local out="$workdir/$name.out.log"
    local err="$workdir/$name.err.log"
    local args=(--state-dir "$state_dir" --port "$port" "$@")
    if [ -n "$interface" ]; then args+=(--interface "$interface"); fi
    "$example" "${args[@]}" >"$out" 2>"$err" </dev/null &
    node_pid=$!
    local waited=0
    while [ "$waited" -lt 120 ]; do
        sleep 0.5
        waited=$((waited + 1))
        if ! kill -0 "$node_pid" 2>/dev/null; then break; fi
        if grep -q 'onoff_node: ready' "$out" 2>/dev/null; then
            node_out="$out"
            return 0
        fi
    done
    echo "the node did not become ready; see $err"
    kill "$node_pid" 2>/dev/null
    exit 1
}

stop_node() {
    if [ -n "$node_pid" ] && kill -0 "$node_pid" 2>/dev/null; then
        kill "$node_pid"
        wait "$node_pid" 2>/dev/null
    fi
    node_pid=""
    sleep 0.5
}

run_steps() {
    MATTER_JS_APP="$app" SHELL_STORAGE="$shell_storage" SHELL_LOG="$shell_log" PAIRING_CODE="$2" \
        node "$here/drive-shell.mjs" "$here/$1"
}

echo '==> starting the node (fresh state)'
start_node node-1 --print-pairing
pairing_code=$(grep -o 'onoff_node: pairing code [0-9]*' "$node_out" | head -n 1 | awk '{print $4}')
if [ -z "$pairing_code" ]; then
    echo 'the node printed no pairing code'
    stop_node
    exit 1
fi

echo '==> commissioning and driving through the shell'
run_steps steps-commission.json "$pairing_code"
first=$?
stop_node

echo '==> restarting the node on its persisted state'
start_node node-2
run_steps steps-restart.json ''
second=$?
stop_node

if [ "$first" -eq 0 ] && [ "$second" -eq 0 ]; then
    echo "PASS: commission, read, toggle, write and restart (logs in $workdir)"
    exit 0
fi
echo "FAIL: commission run $first, restart run $second (logs in $workdir)"
exit 1
