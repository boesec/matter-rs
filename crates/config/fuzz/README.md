# Fuzzing the configuration parser

This directory is a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) crate. It has a
workspace of its own and is excluded from the main one, because it needs a nightly toolchain and
libFuzzer (Linux or macOS; on Windows use WSL).

| Target | What it feeds |
|---|---|
| `config_parse` | Arbitrary bytes as a configuration file to the loader. Loading must return a configuration or a report for every input and never panic. |

## Run it locally

```sh
cargo install --locked cargo-fuzz        # once
rustup toolchain install nightly         # once
cd crates/config
cargo +nightly fuzz run config_parse                            # until stopped
cargo +nightly fuzz run config_parse -- -max_total_time=60      # one minute
```

Where things end up (all git-ignored):

- `fuzz/corpus/config_parse/` - the inputs libFuzzer found interesting. Keep them between runs;
  the next run starts from them.
- `fuzz/artifacts/config_parse/` - one file per crash, hang or out-of-memory input. Re-run a
  single one with `cargo +nightly fuzz run config_parse fuzz/artifacts/config_parse/<file>`. A
  crash input may contain anything; never commit it. Turn the finding into a regular test
  instead.

## How CI runs it

The workflow `.github/workflows/fuzz.yml` finds every `crates/*/fuzz` directory whose
`Cargo.toml` has `[package.metadata] cargo-fuzz = true` and fuzzes every target that
`cargo fuzz list` prints for it, one job per target.

- Pull requests and pushes: 60 seconds per target, starting from the saved corpus.
- Nightly (02:47 UTC) and manual dispatch ("Run workflow" on the Fuzz workflow): 20 minutes per
  target.
- The corpus is kept in the Actions cache, one entry per fuzz crate and target. It is saved even
  when the run fails, and the nightly run on the default branch is what feeds the short runs.
- A crash or hang fails the job. The step summary shows the failing input's path and the last
  lines of the fuzzer output; the `artifacts/<target>/` directory is attached to the run as a
  workflow artefact (kept 14 days) to reproduce the crash locally.
- The nightly compiler is pinned by date in the workflow (`FUZZ_NIGHTLY`) so a new nightly cannot
  break the job by itself. Raise it on purpose.

## Add a target

1. Add `fuzz_targets/<name>.rs` with a `fuzz_target!` and a matching `[[bin]]` entry in
   `Cargo.toml` (copy the `config_parse` one).
2. Nothing else: CI discovers it.

For a new crate, create `crates/<crate>/fuzz` with `cargo fuzz init`, keep
`[package.metadata] cargo-fuzz = true`, keep its own `[workspace]` table, and add the directory to
`exclude` in the root `Cargo.toml`.
