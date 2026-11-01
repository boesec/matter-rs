# Architecture

How matter-rs is put together: the crates, who may depend on whom, how the Matter engine is
driven, what builds without the standard library, and the conventions every crate follows.

## 1. Purpose and reading order

matter-rs is a Matter product in Rust: a bridge first, a controller later, fed through adapters
(MQTT first), configured by one TOML file, running headless as a service. The Matter protocol
itself comes from [rs-matter](https://github.com/project-chip/rs-matter), which matter-rs uses
behind a facade of its own so that the product never depends on one engine's types.

Read in this order:

1. Section 2, the crate map, for what exists and where.
2. Section 3, the dependency rules, before adding a dependency between crates.
3. Section 4, the executor model, before writing anything asynchronous.
4. Sections 5 to 8 for the conventions (`no_std`, allocation, errors, secrets).
5. Section 9 for the Cargo features, and section 10 before adding a crate or an adapter.

The Matter specification is published by the Connectivity Standards Alliance at
<https://csa-iot.org/developer-resource/specifications-download-request/>. The MQTT topic
conventions the MQTT adapter follows are those of
[mqtt-smarthome](https://github.com/mqtt-smarthome/mqtt-smarthome).

## 2. Crate map

All crates live under `crates/`; packages are named `matter-rs-<name>` and live in
`crates/<name>`. The service binary is the package `matter-rs` in `crates/matter-rs` and builds
the executable `matter-rs`. Layers are numbered from the bottom; a crate may depend only on
lower layers (section 3).

| Package | Directory | Layer | `no_std` | Responsibility | Status |
|---|---|---|---|---|---|
| `matter-rs-engine` | `crates/engine` | 0 engine facade | yes | Engine-neutral contract: ids, dynamic values, endpoint descriptions, events, storage contract, errors, `Secret<T>`; fake engine behind the `testing` feature | contract in place; the controller extension follows |
| `matter-rs-engine-rsmatter` | `crates/engine-rsmatter` | 1 engine implementation | no | The only crate that depends on rs-matter; runs it on a dedicated thread | engine in place: node bring-up, dual-stack socket, built-in mDNS, storage over the facade, endpoints and values, commissioning and fabrics, pairing info; dynamic endpoints as far as the pinned revision allows (see its rustdoc) |
| `matter-rs-bridge` | `crates/bridge` | 2 role | yes | Aggregator, bridged devices, endpoint numbering, device types | skeleton; filled by the bridge role |
| `matter-rs-controller` | `crates/controller` | 2 role | yes | Controller role: commission and control devices | planned (release 2) |
| `matter-rs-adapter-api` | `crates/adapter-api` | 3 adapter contract | yes | Trait and types every adapter implements | skeleton; filled by the adapter model |
| `matter-rs-adapter-mqtt` | `crates/adapter-mqtt` | 4 adapter | no | MQTT adapter along mqtt-smarthome | skeleton; filled by the MQTT adapter |
| `matter-rs-config` | `crates/config` | 5 config | no | TOML config model, validation, schema; generated reference in `doc/` | config model, validation, schema and reference in place; adapter and device sections follow with their crates |
| `matter-rs-mgmt-api` | `crates/mgmt-api` | 6 optional | no | Local management API | planned |
| `matter-rs-ui` | `crates/ui` | 6 optional | no | Web UI on top of the management API | planned |
| `matter-rs` | `crates/matter-rs` | 7 service binary | no | Wires everything by features, runs as a service | command line for the configuration in place; the service itself follows |

A skeleton crate compiles, documents its responsibility and layer at the crate root, forbids
unsafe code, inherits the workspace lints, is `publish = false`, and exports its `Error` type
(section 7) and nothing else of substance. The "Status" column is read by a test: a package that
exists must not be called `planned`, and a planned package must not exist.

## 3. Dependency rules

Direct dependencies between workspace crates. "Direct" means named in `[dependencies]`,
`[dev-dependencies]` or `[build-dependencies]` of the crate's manifest, optional or not. Lower
layers never depend on higher ones.

| Crate | May depend on |
|---|---|
| `matter-rs-engine` | nothing in the workspace |
| `matter-rs-engine-rsmatter` | `matter-rs-engine`; the only crate allowed to depend on `rs-matter` and `rs-matter-*` |
| `matter-rs-bridge` | `matter-rs-engine` |
| `matter-rs-controller` | `matter-rs-engine` |
| `matter-rs-adapter-api` | `matter-rs-engine`, `matter-rs-bridge`, `matter-rs-controller` |
| `matter-rs-adapter-mqtt` | `matter-rs-adapter-api`, `matter-rs-engine`, `matter-rs-bridge`, `matter-rs-controller` |
| `matter-rs-config` | `matter-rs-adapter-api`, `matter-rs-adapter-mqtt`, `matter-rs-engine`, `matter-rs-bridge`, `matter-rs-controller` |
| `matter-rs-mgmt-api` | `matter-rs-config` and everything below it |
| `matter-rs-ui` | `matter-rs-mgmt-api` and everything below it |
| `matter-rs` | everything |

Two rules deserve their own sentence:

- **rs-matter isolation.** Only `matter-rs-engine-rsmatter` names `rs-matter` or any `rs-matter-*`
  crate as a direct dependency. No other crate sees an rs-matter type; the engine implementation
  translates everything into facade types and wraps rs-matter errors in its own error type.
- **The engine implementation is reserved for the binary.** "Everything below" in the rows above
  excludes `matter-rs-engine-rsmatter`: roles, adapters, configuration, management API and UI
  program against the facade only, and the service binary is the one place where an engine is
  chosen and wired in. That is what keeps the engine replaceable.

External crates are not restricted by these rules; cargo-deny (`deny.toml`) checks their licences,
advisories and sources.

**How the rules are enforced.** The tests in `crates/matter-rs/tests/` read the workspace
manifests on every test run:

- `dependency_rules.rs` holds the rule table above as code and fails when a workspace crate has a
  direct workspace dependency outside its allowed set, when a crate is missing from the table, or
  when any crate other than the engine implementation depends on rs-matter. The checker is a pure
  function that is unit-tested with fabricated violations, so it is known to fail when it should.
- `architecture_doc.rs` fails when a workspace package is missing from the crate map above, when
  its directory differs, or when the dependency table and the crate map name different crates.
- `workspace_policy.rs` checks the per-crate conventions of section 2 and the `no_std` tier of
  section 5.

## 4. Executor model

rs-matter is built from futures that are not `Send` by default; its optional `send-sync` feature,
which would allow them on a work-stealing executor, is marked work in progress upstream. The
product side (the binary, adapters with network I/O, the management API) runs on tokio. The two
meet like this:

- **The engine runs on a dedicated OS thread** owned by `matter-rs-engine-rsmatter`. That thread
  drives the rs-matter futures (transport, mDNS, responder, interaction model) with a
  single-threaded `block_on`, exactly as rs-matter's own examples do. The thread, its executor
  and the channels into and out of it are implementation details of the engine crate and never
  show in the facade.
- **The facade is async but executor-neutral.** `matter-rs-engine` uses `core::future` only: it
  names no runtime, spawns nothing and holds no runtime handle. Its asynchronous operations are
  plain futures that any executor can poll, so the same contract works on tokio, on an embedded
  executor, or in a test that polls by hand.
- **Calls cross the boundary through the facade.** A product-side task that writes an attribute,
  reports a changed value or raises an event calls the facade; the engine implementation hands
  the call over to the engine thread and returns the result as a future. Nothing on the engine
  thread blocks on the product side, and nothing on the product side holds an engine lock.
- **Library crates depend on tokio only where they do I/O.** The roles and the adapter contract
  are runtime-free; an adapter that opens sockets uses tokio and says so in its manifest. The
  skeleton has no tokio dependency anywhere yet.

A microcontroller target later gets its own engine implementation crate on an embedded executor;
it keeps the same facade.

## 5. The `no_std` tier

Three crates build without the standard library: `matter-rs-engine`, `matter-rs-adapter-api` and
`matter-rs-bridge`. Each is `#![cfg_attr(not(feature = "std"), no_std)]` with
`extern crate alloc`, and each has a `std` feature that is on by default. Product crates on the
std tier depend on them with `std` enabled; an embedded build turns the default features off.
`matter-rs-engine-rsmatter` is a std crate (threads, sockets, files); a microcontroller engine is
a separate crate, not a feature of it.

**How it is checked.** The pinned toolchain in `rust-toolchain.toml` lists the bare-metal target
`thumbv7em-none-eabihf`, so rustup installs it with the toolchain, and the quality gate runs

```sh
cargo check -p matter-rs-engine -p matter-rs-adapter-api -p matter-rs-bridge \
    --no-default-features --target thumbv7em-none-eabihf
```

locally (`scripts/check.sh`, `scripts/check.ps1`) and in CI. A std dependency that slips into one
of the three crates fails this check. `cargo check` needs no linker for the target, so the check
runs anywhere the toolchain does.

## 6. Allocation and buffer strategy

- `alloc` is allowed everywhere, including the `no_std` tier: `Vec`, `String`, `Box` and `Arc`
  are the normal tools, so that the facade can carry dynamic values and endpoint descriptions
  without const-generic capacities leaking into every signature.
- Collections that grow with external input (endpoints, devices, subscriptions, queued messages)
  are bounded. The bound is a capacity that comes from configuration or from a named constant,
  never implicit, and the footprint of each bound is measured as part of the build profiles.
- Library crates make no global choices: no `#[global_allocator]`, no `static mut`, no unsafe.
  The binary and, later, an embedded target choose the allocator.
- Packet and record buffers that rs-matter needs are rs-matter's own and stay inside the engine
  implementation; the facade exchanges owned values, not buffers.

## 7. Error model

- Every library crate has exactly one public error type, `Error`, an enum marked
  `#[non_exhaustive]` that implements `core::error::Error` and `core::fmt::Display`. In the
  `no_std` tier it does so without the standard library (`thiserror` with default features off).
  Adding a variant is therefore not a breaking change.
- A crate wraps the errors of the crates below it in a variant (`Error::Engine(engine::Error)`),
  with the wrapped error as `source`. It never re-exports a lower error as its own, so a caller
  always matches on the error of the crate it called.
- Errors are `Send + Sync + 'static`, because they cross the boundary between the engine thread
  and the product side.
- The binary may use a boxed dynamic error (`Box<dyn std::error::Error>`) at its top level, where
  the only thing left to do with an error is to report it and exit.
- Every library crate pins the contract with a compile-time test (`error_is_core_error`).

## 8. Secrets

Setup passcodes, keys, pairing codes and broker passwords never appear in logs, error messages,
panics, trace files or user-visible output. `matter-rs-engine` provides `Secret<T>` for them:

- `Debug` and `Display` print the fixed marker `[REDACTED]`, which reveals neither the value nor
  its length, so a secret cannot leak by being formatted by accident, including inside a struct
  that derives `Debug`.
- The value is reachable only through the explicit `expose()` method, which makes every use
  visible in the code.
- `Clone` and `PartialEq` are derived; the comparison is not constant-time. Serialisation is added
  with the configuration model.

Where a secret may be shown at all (for example the pairing code on the command line at
commissioning time) is a deliberate `expose()` at that one place.

## 9. Cargo features

Features select what is compiled in, so that a build contains only what it needs. Names are
kebab-case: roles are `role-<name>`, adapters `adapter-<name>`, cluster groups `clusters-<group>`,
and optional parts go by their own name. A feature of the binary forwards to the crates it
selects: `role-bridge` on the binary enables the bridge crate and the `role-bridge` feature of the
adapter contract, of each adapter and of the config crate, which in turn enable their bridge
specific parts.

Features of the binary package `matter-rs`:

| Feature | Default | Selects |
|---|---|---|
| `role-bridge` | on | `matter-rs-bridge` and the bridge parts of the adapter contract, the adapters and the config |
| `adapter-mqtt` | on | `matter-rs-adapter-mqtt` and its config section |
| `role-controller` | off | `matter-rs-controller` and the controller parts above it (release 2; added with the crate) |
| `mgmt-api` | off | `matter-rs-mgmt-api`, the local management API (added with the crate) |
| `ui` | off | `matter-rs-ui`, which implies `mgmt-api` (added with the crate) |
| `clusters-<group>` | per group | A group of cluster implementations in the bridge role; the groups are defined with the bridge |

Library crates carry only the features that the binary forwards to them, plus `std` on the
`no_std` tier. The binary builds with its defaults and with
`--no-default-features --features role-bridge`; CI checks both, so that a minimal build selects
only its own features.

## 10. Adding a crate or an adapter

**A new crate:**

1. Create `crates/<name>` with package name `matter-rs-<name>`, `publish = false`,
   `[lints] workspace = true`, and the workspace `version`, `edition`, `rust-version`,
   `license` and `authors`.
2. Put `#![forbid(unsafe_code)]` and `#![doc(test(attr(forbid(unsafe_code))))]` at the crate root,
   with a doc comment that states the crate's responsibility and layer. On the `no_std` tier add
   `#![cfg_attr(not(feature = "std"), no_std)]`, `extern crate alloc;` and the default `std`
   feature, and add the crate to the `no_std` check in both scripts and in CI.
3. Define the crate's `Error` as in section 7, with a variant per lower crate it wraps, and the
   `error_is_core_error` test.
4. Add the crate to the crate map (section 2), to the dependency table (section 3) and to the rule
   table in `crates/matter-rs/tests/dependency_rules.rs`. The tests fail until all three agree.
5. Add a path entry under `[workspace.dependencies]` and, if the binary selects the crate, a
   feature on the binary that forwards to it (section 9).

**A new adapter** is a crate `matter-rs-adapter-<name>` on layer 4 that implements the adapter
contract of `matter-rs-adapter-api`, depends on nothing above that contract, carries the `role-*`
features it supports, and is selected by a feature `adapter-<name>` on the binary that also
enables its config section in `matter-rs-config`. Adapters are compiled in, never loaded at run
time.
