# matter-rs

A Matter bridge and controller written in Rust.

> **Status: early development.** Nothing here is usable yet; interfaces and layout will change.

## What it is

matter-rs is a Matter product in Rust from which a **bridge** and, later, a **controller** are
built. It aims to be universal:

- **Adapters.** Data enters and leaves through adapters. MQTT comes first and is the universal
  route for external programs; topics follow the
  [mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) conventions.
- **Config first.** The service is configured through one TOML file and runs headless: as a systemd
  service or in a container on Linux, in the foreground on Linux and Windows. A web UI is planned but strictly optional: everything it does can
  be done through the config file and the command line.
- **Only what you need.** Parts that are not needed can be left out at compile time through Cargo
  features and switched off at run time, so the same code base fits a server, a small Linux board
  and, later, a microcontroller.

Under the hood matter-rs uses [rs-matter](https://github.com/project-chip/rs-matter) as its Matter
engine, behind a facade of its own. rs-matter is a protocol toolkit; matter-rs builds the product
on top of it: a dynamic bridge, the adapter model, configuration, service integration and build
profiles for small targets.

The engine is pinned to one git revision of rs-matter (0.4.1 plus the unreleased fixes up to that
commit) in the workspace manifest; only the crate `matter-rs-engine-rsmatter` depends on it. That
crate runs rs-matter on a dedicated thread, binds a dual-stack UDP socket so that IPv4 works on
Windows too, announces the node with rs-matter's built-in mDNS responder, keeps rs-matter's state
in the same directory as the product's, and turns commands and writes from Matter clients into
events of the facade. Its rustdoc describes the upgrade procedure, what this revision can do for
endpoints that come and go at run time, and the audit of its logs for secrets. A scripted
scenario against the matter.js shell lives in [`tests/interop-local/`](tests/interop-local/).

Why Rust: low overhead, memory safety without a garbage collector, one static binary per target,
and a `no_std` path to embedded devices.

How the crates fit together, who may depend on whom and which conventions every crate follows is
described in [ARCHITECTURE.md](ARCHITECTURE.md).

### Planned releases

| Release | Content |
|---|---|
| Bridge | A commissionable Matter bridge with bridged devices over IP, fed through the MQTT adapter, configured by file, running as a systemd service or container on Linux and in the foreground on Linux and Windows. |
| UI | An optional web UI for configuration that writes the same config file. |
| Controller | Commission and control Matter devices and expose them through adapters such as MQTT. |
| Later | BLE commissioning, a microcontroller port, OTA updates, more device types and adapters. |

### Not a goal

- **Certification.** matter-rs uses the test vendor id `0xFFF1` and test attestation
  certificates. The design does not prevent certification later.
- **Thread radio.** Thread devices are reached through an external border router.
- **Native plugins loaded at run time.** Adapters are Rust crates selected at compile time.

## Building

The toolchain is pinned in `rust-toolchain.toml`; [rustup](https://rustup.rs) installs it on the
first build. The minimum supported Rust version is 1.92.

```sh
cargo build --workspace
```

The engine facade, the adapter contract and the bridge role also build without the standard
library. That `no_std` tier is checked against a bare-metal target, which rustup installs with the
pinned toolchain:

```sh
cargo check -p matter-rs-engine -p matter-rs-adapter-api -p matter-rs-bridge \
    --no-default-features --target thumbv7em-none-eabihf
```

## Usage

The service is configured by one TOML file. The binary `matter-rs` writes, checks and documents
it, and runs the node (see [Running as a service](#running-as-a-service)).

```sh
matter-rs init --output config.toml   # commented starter file with a fresh passcode and discriminator
matter-rs validate config.toml        # every error and warning as <file>:<line>:<column>
matter-rs features                    # roles and adapters compiled into this binary
matter-rs schema                      # JSON schema of the file
matter-rs schema --markdown           # configuration reference
matter-rs pairing-info config.toml    # manual pairing code, QR code text and discriminator
```

`pairing-info` is the one place the pairing code is shown: it encodes the setup passcode, so
neither `validate` nor the logs ever print it.

Without an explicit path the file is taken from `--config <path>`, then from the environment
variable `MATTER_RS_CONFIG`, then from the platform default (`/etc/matter-rs/config.toml` on
Linux, `%ProgramData%\matter-rs\config.toml` on Windows). `init` refuses to overwrite an existing
file unless `--force` is given. Exit codes: 0 success (warnings allowed), 1 an invalid or
unreadable file or a failed command, 2 wrong usage.

Secrets such as the setup passcode can be written into the file, or read from an environment
variable (`passcode = { env = "MATTER_RS_PASSCODE" }`) or a file
(`passcode = { file = "/etc/matter-rs/passcode" }`). Nothing ever prints them.

Every section and key is described in the [configuration reference](doc/config.md); the
[JSON schema](doc/config.schema.json) lets editors check the file while it is written. Both are
generated from the code, and a test fails when they are out of date.

## Running as a service

`matter-rs run` starts the node from the configuration file: the Matter engine (rs-matter, on a
thread of its own) with the bridge role on it, until it is stopped. The bridge does not have
adapters yet, so it serves its aggregator without bridged devices; configured adapter instances
are reported in the log and not started.

```sh
matter-rs init --output config.toml
matter-rs run --config config.toml
matter-rs pairing-info config.toml    # in another terminal: the codes for the commissioner
```

When the node is not commissioned yet, the log says so and points to `matter-rs pairing-info`;
the log itself never contains the passcode or a pairing code. Fabrics, keys, counters, the
bridge's endpoint numbers and a generated unique id of the node are kept in the state directory
(`[state] dir`, by default `/var/lib/matter-rs` for root and `~/.local/state/matter-rs` otherwise
on Linux, `%ProgramData%\matter-rs` or `%LOCALAPPDATA%\matter-rs` on Windows), so a restart
keeps the fabrics without commissioning again.

**Stopping.** SIGTERM and SIGINT (Unix), Ctrl-C, Ctrl-Break and closing the console (Windows)
stop the node cleanly: the adapters stop, then the bridge, then the engine, which writes its
remaining state before the process exits with code 0. A second signal during the shutdown, or a
shutdown that takes longer than 15 seconds, ends the process with code 1. Exit code 1 also means
the node could not start (invalid configuration, port in use, engine failure); the reason is on
standard error and in the log. `matter-rs run --help` lists all of this.

**Logs.** The log goes to standard error as text, or as one JSON object per line with
`[log] format = "json"`. `[log] level` sets the level; a non-empty `RUST_LOG` replaces it with a
full filter, for example `RUST_LOG=info,rs_matter=debug`. The engine's and rs-matter's own log
lines go through the same log with their targets and levels. On Linux, `[log] journald = true`
writes to the systemd journal directly (the default build contains the Cargo feature `journald`).

**Linux, systemd.** [`packaging/systemd/matter-rs.service`](packaging/systemd/matter-rs.service)
runs `/usr/bin/matter-rs run --config /etc/matter-rs/config.toml` as the system user
`matter-rs`, with `/var/lib/matter-rs` as its state directory (created by systemd), a 20-second
stop timeout, restart on failure, and hardening that keeps UDP 5540 (Matter) and 5353 (multicast
DNS) working without any capability. The comment at the top of the unit lists the installation
steps. Its log is in the journal: `journalctl -u matter-rs`.

**Container.** [`packaging/container/Dockerfile`](packaging/container/Dockerfile) builds the
binary with the pinned toolchain and ships it on `debian:bookworm-slim`, running as an
unprivileged user. The configuration is read from `/etc/matter-rs/config.toml`, the state lives
in the volume `/var/lib/matter-rs`. The container needs the host network, because commissioning
finds the node over multicast DNS, which does not cross Docker's bridge network:

```sh
docker build -f packaging/container/Dockerfile -t matter-rs .
docker run -d --name matter-rs --network host \
    -v /srv/matter-rs/config.toml:/etc/matter-rs/config.toml:ro \
    -v matter-rs-state:/var/lib/matter-rs \
    matter-rs
```

`docker stop` sends SIGTERM and stops the node cleanly. The image is built and smoke-tested for
`linux/amd64` and `linux/arm64` in CI; it is not published.

**Windows.** `matter-rs run` runs in the foreground in a console; there is no Windows service.

## Quality gate

The same checks run locally and in CI: format check, clippy with warnings as errors, tests, the
`no_std` check above, a build of the binary with its default features and with only the bridge
role, and a dependency check with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)
(licences, advisories, duplicate crates, sources).
The parsers we write have fuzz targets that run in a separate workflow, briefly on every push and
for longer every night; see [crates/config/fuzz](crates/config/fuzz/README.md).

```sh
cargo install --locked cargo-deny   # once
./scripts/check.sh                  # Linux, macOS
```

```powershell
./scripts/check.ps1                 # Windows
```

## Licence

Licensed under the [Apache License, Version 2.0](LICENSE). See [NOTICE](NOTICE) for attributions.

Matter is a trademark of the Connectivity Standards Alliance. matter-rs is an independent project
and is neither affiliated with nor certified by the Connectivity Standards Alliance.
