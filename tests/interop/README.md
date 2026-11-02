# Interop test harness

Scripted interoperability scenarios against a Matter device under test (DUT), driven by the
reference controllers in pinned Linux containers:

- **chip-tool** and the **Python test framework** of connectedhomeip, from the official
  certification image (`connectedhomeip/chip-cert-bins`);
- the **matter.js shell** as a second controller, from a small image built here with the pinned
  `@matter/*` packages.

Two devices under test run side by side, each on its own port and discriminator and each with
its own chip-tool fabric:

- a **matter.js on/off light** from the same image (`dut`, `chiptool`, `commissioned_dut`), the
  reference device that shows the harness itself works;
- the **engine example `onoff_node`** of this repository, built on the runner from
  `crates/engine-rsmatter/examples/onoff_node.rs` and started natively (`engine_dut`,
  `engine_chiptool`, `engine_commissioned_dut`). It takes orders on its standard input, so the
  scenarios can add and remove endpoints while chip-tool watches; see
  [The engine example as device under test](#the-engine-example-as-device-under-test).

Any other program takes the matter.js light's place by changing one fixture parameter (see
[Swapping the DUT](#swapping-the-device-under-test)); the scenarios do not change.

The runner is pytest. Every scenario has a timeout, every process the harness starts writes its
output to `logs/`, and every assertion that fails on a counterpart carries the last lines of the
relevant log. The workflow `.github/workflows/interop.yml` runs the suite on GitHub Actions and
uploads `logs/` as an artefact.

## Layout

| Path | Purpose |
|---|---|
| `conftest.py` | Fixtures: configuration, redactor, harness log, both DUTs, their chip-tools, the commissioned DUTs |
| `harness/` | The harness: process control with log collection (`process.py`), redaction (`redact.py`), chip-tool with its interactive session (`chiptool.py`), the engine DUT driver (`enginedut.py`), the shell (`shell.py`), the Python framework runner (`pythontc.py`), pins and configuration (`config.py`), manual pairing code (`pairing.py`) |
| `scenarios/test_*.py` | The scenarios (below); `test_engine.py` holds the engine DUT scenarios |
| `selftest/test_*.py` | Tests of the harness itself; run without Docker |
| `containers/images.toml` | Every image with tag and digest; the harness pulls by digest |
| `containers/matterjs/` | `Dockerfile`, `package.json`, `package-lock.json` and `device.mjs` of the matter.js image |
| `pics/` | Hand-written PICS files, one per DUT (`matterjs-onoff.pics`, `engine-onoff-node.pics`) |
| `requirements.txt`, `pyproject.toml` | Pinned Python tools; pytest and ruff configuration |
| `logs/` | Collected logs of the last run (git-ignored) |

## Pinned revisions

| Component | Pin |
|---|---|
| connectedhomeip certification image | `connectedhomeip/chip-cert-bins:e91ea83caae4adb1871aa4bbe93c4be2dd9e1abf`, manifest digest `sha256:81a2b68f46e52e8352527a67c6487e0da57fdb166490e6af091d6e4f3974d9db`, `linux/arm64`, built 2026-08-19 |
| connectedhomeip commit | `e91ea83caae4adb1871aa4bbe93c4be2dd9e1abf` ("Update DM at 1.6.1"), the SDK tag of the official Matter 1.6.1 Test Harness |
| matter.js | `@matter/main` and `@matter/nodejs-shell` **0.18.0** (`containers/matterjs/package-lock.json`) |
| Node base image | `node:22-bookworm-slim`, digest `sha256:4b150b48b4116e2a22f2c39cc27025fbf4f153e2078fd3b14d159fdb5c7279da` |
| Python tools | `requirements.txt`: pytest 9.2.0, pytest-timeout 2.4.0, PyYAML 6.0.3, ruff 0.17.3 |

Why this connectedhomeip commit and not the tip of `v1.6-branch` that rs-matter pins for its own
integration tests (`f3af82eb6fd1aed968e04642cb8d2ac305f5a371`): the certification image is only
published for the commits the Test Harness releases use; there is none for that commit, and
building the image (chip-tool plus some 35 example apps plus the Python wheels) takes hours on a
hosted runner. The Test Harness 1.6.1 tag is the closest published image on the 1.6 line. Moving
the pin means changing `containers/images.toml` (and this table).

The digests were read from the registries' manifests on 2026-11-01. The self-test
`selftest/test_config.py` checks that every image has a digest and that the matter.js
`Dockerfile` uses the same Node digest as `images.toml`.

## Scenarios

| Test | What it proves |
|---|---|
| `test_chiptool_commission_and_toggle` | chip-tool commissions the DUT over IP (`pairing onnetwork-long`), toggles `OnOff` on the light endpoint and the read-back value flipped |
| `test_matterjs_shell_commission_and_toggle` | chip-tool opens an enhanced commissioning window on the same DUT; the matter.js shell commissions it as a **second fabric** with the fresh pairing code, toggles it, reads it back; chip-tool then reads the same value through the first fabric |
| `test_python_tc_smoke` | chip-tool opens another window; `TC_CGEN_2_1.py` (General Commissioning attributes) runs through the connectedhomeip Python framework with `pics/matterjs-onoff.pics` as a third fabric; the Mobly `test_summary.yaml` is parsed and the test's record must be `PASS` with zero failures and errors, in addition to the exit code |
| `test_harness_detects_failure` | a DUT that starts but never prints its ready pattern fails within its 10 s ready timeout; the assertion message contains the DUT's last log lines and the container is gone afterwards |
| `test_logs_contain_no_passcode` | runs last; no file under `logs/` contains the configured passcode, the manual pairing code or an onboarding payload (`MT:...`); the DUT log shows that redaction took place |

The scenarios share one DUT and one chip-tool fabric (session fixtures), in this order: chip-tool
first, then the shell, then the Python test. If chip-tool's commissioning fails, every scenario
that builds on it reports that same error.

### The engine example as device under test

`scenarios/test_engine.py` runs against the engine example, started natively from the binary
`INTEROP_ENGINE_DUT` names, on a temporary state directory, port `INTEROP_ENGINE_DUT_PORT`
(5541) and discriminator `INTEROP_ENGINE_DISCRIMINATOR` (3841), with the same passcode as the
matter.js light. The example starts with its aggregator at endpoint 1 and one on/off light at
endpoint 2 and takes a line protocol on standard input (documented in full in the example's
source): `add <endpoint> <device-type> [parent]`, `remove <endpoint>`,
`set <endpoint> <cluster> <attribute> <value>`, `reachable <endpoint> true|false`, `pairing`
and `quit`, each answered `ok` or `err <reason>`; every engine event is printed as an
`event <kind> ...` line. Device types: `on-off-light`, `dimmable-light`, `contact-sensor`,
`temperature-sensor`. `harness/enginedut.py` sends the commands, waits for the answers and the
events, and restarts the process on the same state directory. The example prints no secret
unless asked with `pairing`; the harness never asks, it derives the pairing code itself.

The scenarios run in file order on one engine DUT and one chip-tool fabric; every scenario that
adds endpoints removes them again, so the conformance scripts at the end see the node as it
started. chip-tool subscriptions live in an interactive session (`chip-tool interactive start`
in a long-lived container on the same fabric), because a plain `subscribe` command exits as soon
as the subscription is established.

| Test | What it proves |
|---|---|
| `test_engine_chiptool_commission_and_toggle` | chip-tool commissions the node over IP; a toggle reaches the product as a `CommandInvoked` event (printed by the DUT) and the read-back value flipped |
| `test_engine_write_accept_and_reject` | a write of `NodeLabel` the product accepts is stored and read back; one it refuses with `ConstraintError` reaches chip-tool as that status and leaves the value |
| `test_engine_restart_keeps_fabric` | after `quit` and a restart on the same state directory the node reports itself commissioned and chip-tool reads and toggles without commissioning again |
| `test_engine_subscription_reports_set_attribute` | with chip-tool subscribed to `OnOff` (max interval 10 s), a `set` on the DUT produces a report within 30 s |
| `test_engine_runtime_added_endpoint_is_served` | an endpoint added while commissioned answers reads, appears in the aggregator's and the root's `PartsList`, and `ConfigurationVersion` increased |
| `test_engine_wildcard_subscription_sees_added_endpoint` | with a wildcard subscription running, `add` produces a report for the new endpoint |
| `test_engine_removed_endpoint_is_unsupported` | after `remove`, a read answers `UnsupportedEndpoint`, `PartsList` no longer lists the endpoint, and a subscription to it is told |
| `test_engine_python_conformance` | `TC_DeviceBasicComposition.py` (with `TC_DESC_2_1` and `TC_DESC_2_2`) and `TC_BRBINFO_2_1.py` run through the framework with `pics/engine-onoff-node.pics`; the first commissions the node as another fabric, the second runs on that fabric; the parsed summaries must show no failure or error, the named cases `PASS`, and every skipped record is listed with the framework's reason |

All eight engine scenarios pass on the runner; none is marked as an expected failure. Two
findings from the first runs shape how the conformance scenario is arranged:

- **The framework reuses its PASE session.** When the Python framework commissions the node
  itself and runs the composition script in the same process, its controller keeps using the
  PASE session (`Using PASE connection`), and `TC_IDM_10_1`'s event subscription over it fails
  with `Not connected`, because the node has dropped that session by then; the test tolerates
  the reference SDK's `UnsupportedAccess` on the same session. The scenario therefore
  commissions with `TC_BRBINFO_2_1` and runs `TC_DeviceBasicComposition` on the established
  fabric over CASE, where `TC_IDM_10_1` passes.
- **The framework removes other administrators' fabrics.** At the end of every test the
  framework's cleanup removes every fabric that was not in the baseline it captured at setup,
  which after its own commissioning means every other fabric, chip-tool's included (the same
  happens on the matter.js device after `TC_CGEN_2_1`). The conformance scenario runs last on
  the engine DUT for that reason; a scenario that needs chip-tool's fabric on this node after a
  framework run would fail at session setup.

`selftest/` covers the harness without Docker: the manual pairing code against the well-known
development vector (`20202021`/`3840` gives `34970112332`), redaction and secret capture, process
control with timeouts, configuration and DUT swapping, parsing of the framework summary.

## The runner on GitHub Actions

**Runner:** `ubuntu-24.04-arm`, a GitHub-hosted arm64 runner (free for public repositories), not
`ubuntu-latest`. The certification image is published for `linux/arm64` only (every tag on Docker
Hub has an arm64 manifest and no amd64 one; checked on 2026-11-01). On an amd64 runner the image
would run under QEMU user emulation, which is slow and an unverified configuration for a Matter
stack; building an amd64 image ourselves is the hours-long job named above. The arm64 runner runs
it natively. The runner image has Docker, Python 3.12 and passwordless `sudo`.

**Networking (the configuration the workflow sets up):**

1. IPv6 is enabled in the kernel; the default-route interface has a link-local address only. The
   workflow enables IPv6 on all interfaces (`net.ipv6.conf.*.disable_ipv6=0`) and adds a
   unique-local address `fd12:3456:789a::1/64` to the default-route interface, so the DUT and the
   controllers have a non-link-local IPv6 address to advertise and to connect to. The chip-tool
   and Python controller builds in the certification image are `ipv6only`.
2. `avahi-daemon` is installed from Ubuntu's packages with `use-ipv6=yes` and `use-ipv4=yes` and
   restarted. The certification image's chip-tool and Python controller use platform mDNS: they
   are Avahi clients and need the host's D-Bus socket, which the harness mounts into every
   container (`/var/run/dbus/system_bus_socket`).
3. Every container runs with `--network host`, so the containers, Avahi and the DUT share one
   network namespace and multicast DNS stays on the host.
4. The matter.js shell and device use matter.js's own mDNS stack (no Avahi); they bind UDP 5353
   next to Avahi with address reuse.

**Verified on the runner** (`ubuntu-24.04-arm`, first green run on 2026-11-01): the whole job
takes about two minutes, of which the scenarios themselves take about 40 seconds (image pull
about 40 seconds, matter.js image build about 15 seconds). What had to be true for the four
scenarios to pass, each found by a red run before it:

- IPv6 enabled in the kernel plus a unique-local address on the default interface, and
  `avahi-daemon` with `use-ipv6=yes`: the certification image's chip-tool and Python controller
  are IPv6-only and resolve the device through Avahi.
- The containers that talk to Avahi run with `--security-opt apparmor=unconfined`. Under Docker's
  default profile the host's D-Bus answers "Access denied" and chip-tool fails before any
  networking with "Open file failed" from its mDNS initialisation; the workflow's probe step
  shows both variants.
- The Python framework gets `--paa-trust-store-path /paa` with the test roots from `paa/`;
  its default store next to its scripts is empty in the image, and attestation fails.
- Each enhanced commissioning window is revoked before a new one is opened, so one scenario's
  leftover window cannot make the next answer Busy.
- The matter.js shell is started by its ESM entry point; the package's bin link points to a
  CommonJS build the published tarball does not contain.

**The engine example on the runner:** the job installs the pinned toolchain from
`rust-toolchain.toml` (rustup is on the image), restores the cargo registry, git and
`target/release` caches keyed on `Cargo.lock` and the toolchain (the sources as a secondary key),
builds `onoff_node` with `cargo build --release` and hands its path (`INTEROP_ENGINE_DUT`) and
the default-route interface (`INTEROP_ENGINE_INTERFACE`) to the harness. The example runs
natively with host networking next to Avahi; its built-in mDNS responder shares port 5353 through
address reuse, like the matter.js device.

**Triggers:** pull requests, pushes to every branch, a nightly schedule (03:17 UTC) and manual
dispatch. One job runs both device sets in about 7 minutes without a warm cargo cache (measured
2026-11-02: 429 s in all, of which the release build of the example takes 186 s, the scenarios
136 s, the certification image pull 41 s); with the cache restored the build shrinks to the
changed crates. The step summary prints the build and scenario durations of every run. Were the pull-request trigger to exceed 20 minutes, it would
keep only the chip-tool scenarios of both devices and move the rest to the schedule; that split
is not needed.

**Diagnosing a run:** the step "Run the interop scenarios" shows the pytest output (live log
lines of the harness are redacted like the files). The step "Print the last lines of every
collected log" prints the tail of each file under `logs/` in collapsible groups, the step summary
lists the per-test results and the runner's IPv6 addresses, and the artefact
`interop-logs-<run id>-<attempt>` holds the complete `logs/` directory.

## Running locally

The harness runs on Linux with Docker (for this project: Debian in WSL with Docker Engine).
Windows is not a target; only `selftest/` runs there.

The certification image is arm64. On an amd64 machine Docker runs it under QEMU user emulation
after `docker run --privileged --rm tonistiigi/binfmt --install arm64`; this is untested and
slow, so expect to raise `INTEROP_READY_TIMEOUT` and the per-command timeouts. On an arm64
machine (Raspberry Pi 4 or 5 with a 64-bit OS, an Apple-silicon Linux VM) it runs natively.

```bash
cd tests/interop
python3 -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt

# host services: IPv6 on the interface, Avahi with IPv6
sudo sysctl -w net.ipv6.conf.all.disable_ipv6=0
sudo apt-get install -y avahi-daemon avahi-utils
sudo sed -i -E 's/^#?use-ipv6=.*/use-ipv6=yes/' /etc/avahi/avahi-daemon.conf
sudo systemctl restart avahi-daemon

# images: the certification image by digest, the matter.js image built here
docker pull "$(PYTHONPATH=. python -c 'from harness.config import load_images; print(load_images()["chip"].pinned)')"
docker build -t matter-rs-interop-matterjs:local containers/matterjs

# the whole suite (self-tests, then the scenarios), logs in ./logs
INTEROP_REQUIRE_DOCKER=1 python -m pytest -v

# only the self-tests, or only one scenario
python -m pytest selftest
INTEROP_REQUIRE_DOCKER=1 python -m pytest scenarios/test_chiptool.py

# the engine example as second device: build it, then name it
cargo build --release -p matter-rs-engine-rsmatter --example onoff_node
INTEROP_ENGINE_DUT=../../target/release/examples/onoff_node INTEROP_ENGINE_INTERFACE=eth0 \
INTEROP_REQUIRE_DOCKER=1 python -m pytest -v
```

Without Docker (or without the images) the Docker scenarios are skipped; `INTEROP_REQUIRE_DOCKER=1`
turns that into a failure, which is what the workflow sets.

WSL note: Docker Engine inside the Debian WSL distribution shares the WSL network namespace;
`systemctl` needs systemd enabled in `/etc/wsl.conf`. Avahi inside WSL has not been verified by
the project yet.

## Configuration

All settings are environment variables with defaults (`harness/config.py`):

| Variable | Default | Meaning |
|---|---|---|
| `INTEROP_PASSCODE` | `20202021` | Setup passcode of the DUT (a secret for the logs) |
| `INTEROP_DISCRIMINATOR` | `3840` | Long discriminator of the DUT |
| `INTEROP_VENDOR_ID`, `INTEROP_PRODUCT_ID` | `0xFFF1`, `0x8000` | Test vendor and product id |
| `INTEROP_DUT_PORT` | `5540` | UDP port of the DUT |
| `INTEROP_CHIP_IMAGE` | from `images.toml` | Certification image reference |
| `INTEROP_MATTERJS_IMAGE` | `matter-rs-interop-matterjs:local` | The matter.js image built from `containers/matterjs` |
| `INTEROP_DBUS_SOCKET` | `/var/run/dbus/system_bus_socket` | Host D-Bus socket for Avahi |
| `INTEROP_LOGS_DIR` | `tests/interop/logs` | Where logs are collected (emptied at session start) |
| `INTEROP_READY_TIMEOUT` | `90` | Seconds to wait for the DUT's ready pattern |
| `INTEROP_REQUIRE_DOCKER` | unset | `1`: missing Docker or images fail instead of skip |
| `INTEROP_RUN_ID` | random | Suffix of container and volume names (`interop-<id>-<component>`) |
| `INTEROP_ENGINE_DUT` | unset | Path of the built `onoff_node` example; without it the engine scenarios skip (fail with `INTEROP_REQUIRE_DOCKER=1`) |
| `INTEROP_ENGINE_INTERFACE` | unset | Network interface the engine example announces on (`--interface`); the engine chooses one when unset |
| `INTEROP_ENGINE_DUT_PORT` | `5541` | UDP port of the engine example |
| `INTEROP_ENGINE_DISCRIMINATOR` | `3841` | Long discriminator of the engine example |

### Swapping the device under test

The DUT is a command line plus a ready pattern. The default starts the matter.js light in its
container. Any other program, for example our own binary running natively on the runner, takes
its place through three variables:

```bash
INTEROP_DUT_COMMAND="target/debug/matter-rs --config tests/interop/dut.toml" \
INTEROP_DUT_READY_PATTERN="bridge is online" \
INTEROP_DUT_ENDPOINT=1 \
INTEROP_REQUIRE_DOCKER=1 python -m pytest scenarios
```

The command is split like a shell would; the ready pattern is a regular expression matched against
each output line; the endpoint is the one with the On/Off cluster the scenarios toggle. The DUT
must announce itself with the configured passcode and discriminator. A PICS file for the new DUT
goes into `pics/` and is referenced from the Python scenario.

## Logs and secrets

Every process the harness starts writes to a file under `logs/` (`dut/` for both devices,
`chip-tool/`, `matterjs-shell/`, `python-tc/`, `broken-dut/`, and for the engine device
`engine-chip-tool/` with the interactive sessions and `engine-python-tc/`), the harness itself
to `logs/harness.log`, and the workflow adds `logs/pytest.log`. All of it passes through one
redactor:

- the configured passcode and the manual pairing code derived from it are masked as `<redacted>`;
- onboarding payloads (`MT:...`) and any 11- or 21-digit number (the shape of a manual pairing
  code) are masked without configuration;
- a pairing code that a tool creates at run time (chip-tool's enhanced commissioning window) is
  captured from the raw output line, registered as a secret and handed to the next controller
  without ever being written;
- the files the Python framework writes itself (`python-tc/framework/`) are redacted in place
  after the run.

`test_logs_contain_no_passcode` checks the result at the end of every run.

## Troubleshooting

- *chip-tool pairing times out, no device discovered:* check that `avahi-daemon` runs on the host
  with `use-ipv6=yes`, that the D-Bus socket path exists, and that the DUT log shows an IPv6
  address being announced. `avahi-browse -a -t` on the host must list `_matterc._udp` while the
  DUT is commissionable.
- *the shell hangs after `commission pair`:* the shell downloads the Distributed Compliance
  Ledger's certificates on first use; the run needs outbound internet access. The matter.js device
  uses the Matter test PAA: chip-tool trusts it from its built-in test store, the Python
  framework from the roots in `paa/` (mounted as `/paa`, see `paa/README.md`), and the shell
  reports it as a "PaaNotTrusted" finding and continues.
- *the Python framework fails with "Failed Device Attestation":* its trust store is not set or
  empty; the harness passes `--paa-trust-store-path /paa`. A device with another test chain needs
  its root added to `paa/`.
- *chip-tool or the framework fail at start with "Open file failed" from the mDNS code:* the
  container could not connect to the host's D-Bus (Avahi answers "Access denied" under Docker's
  default AppArmor profile). The harness starts those containers with `apparmor=unconfined`; the
  workflow's probe step shows both variants.
- *`docker rm` or `volume rm` errors at teardown:* harmless, the names are per run; stale ones
  are listed by `docker ps -a --filter name=interop-` and `docker volume ls --filter name=interop-`.
