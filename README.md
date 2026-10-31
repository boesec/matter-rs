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

## Quality gate

The same checks run locally and in CI: format check, clippy with warnings as errors, tests, and a
dependency check with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) (licences,
advisories, duplicate crates, sources).

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
