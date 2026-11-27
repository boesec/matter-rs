#!/usr/bin/env bash
# Smoke test of a built container image: the binary runs, writes a starter configuration that it
# accepts, and the service starts on it and stops cleanly on SIGTERM.
#
# Usage: packaging/container/smoke.sh IMAGE
set -euo pipefail

image="${1:?usage: smoke.sh IMAGE}"
name="matter-rs-smoke-$$"

cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "== matter-rs --version"
docker run --rm "$image" --version

echo "== matter-rs init and validate"
docker run --rm --entrypoint sh "$image" -c \
    'matter-rs init --output /tmp/c.toml && matter-rs validate /tmp/c.toml'

echo "== matter-rs features"
docker run --rm "$image" features

echo "== matter-rs run, stopped with SIGTERM"
# The configuration is written inside the container, so that it belongs to the service user;
# the state goes to the image's state volume. The bridge network is enough to start the node.
docker run -d --name "$name" --entrypoint sh "$image" -c \
    'matter-rs init --output /tmp/c.toml >/dev/null && exec matter-rs run --config /tmp/c.toml'
for _ in $(seq 1 60); do
    if docker logs "$name" 2>&1 | grep -q "node is running"; then
        break
    fi
    sleep 1
done
docker logs "$name" 2>&1 | grep "node is running"
docker stop --time 20 "$name" >/dev/null
code=$(docker inspect --format '{{.State.ExitCode}}' "$name")
docker logs "$name" 2>&1 | tail -n 5
if [ "$code" != "0" ]; then
    echo "matter-rs run exited with code $code after SIGTERM" >&2
    exit 1
fi
docker logs "$name" 2>&1 | grep -q "matter-rs stopped"
echo "smoke test passed"
