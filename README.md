# matter-rs

A Matter bridge and controller written in Rust.

> **Status: early development.** Nothing here is usable yet; interfaces and layout will change.

## What it is

matter-rs is a Matter product in Rust from which a **bridge** and, later, a **controller** are
built. It aims to be universal:

- **Adapters.** Data enters and leaves through adapters. MQTT comes first and is the universal
  route for external programs; topics follow the
  [mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome) conventions.
- **Config first.** The service is configured through one TOML file and runs headless as a system
  service on Linux and Windows. A web UI is planned but strictly optional: everything it does can
  be done through the config file and the command line.
- **Only what you need.** Parts that are not needed can be left out at compile time through Cargo
  features and switched off at run time, so the same code base fits a server, a small Linux board
  and, later, a microcontroller.

Under the hood matter-rs uses [rs-matter](https://github.com/project-chip/rs-matter) as its Matter
engine, behind a facade of its own. rs-matter is a protocol toolkit; matter-rs builds the product
on top of it: a dynamic bridge, the adapter model, configuration, service integration and build
profiles for small targets.

Why Rust: low overhead, memory safety without a garbage collector, one static binary per target,
and a `no_std` path to embedded devices.

How the crates fit together, who may depend on whom and which conventions every crate follows is
described in [ARCHITECTURE.md](ARCHITECTURE.md).

### Planned releases

| Release | Content |
|---|---|
| Bridge | A commissionable Matter bridge with bridged devices over IP, fed through the MQTT adapter, configured by file, running as a service on Linux and Windows. |
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
it; running the service follows later.

```sh
matter-rs init --output config.toml   # commented starter file with a fresh passcode and discriminator
matter-rs validate config.toml        # every error and warning as <file>:<line>:<column>
matter-rs features                    # roles and adapters compiled into this binary
matter-rs schema                      # JSON schema of the file
matter-rs schema --markdown           # configuration reference
```

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

## Quality gate

The same checks run locally and in CI: format check, clippy with warnings as errors, tests, the
`no_std` check above, a build of the binary with its default features and with only the bridge
role, and a dependency check with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny)
(licences, advisories, duplicate crates, sources).

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
