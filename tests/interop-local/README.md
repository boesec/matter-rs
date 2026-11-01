# Local interop scenario: matter.js shell against the On/Off node example

A scripted end-to-end check of the rs-matter engine on a developer machine: the example
`onoff_node` of `crates/engine-rsmatter` is commissioned by the matter.js shell (a Matter
controller), read, toggled, written to, restarted on its persisted state and read again. It
complements the unit and contract tests, which need no Matter client, and runs by hand, not in
CI. The CI interop harness lives in `tests/interop/`.

## What it proves

| Step | Checks |
|---|---|
| `commission pair` with the manual code the example prints | discovery over mDNS, PASE, attestation with the test credentials, CASE, `pairing_info()` matches the shell's expectation |
| read `Descriptor.PartsList` of the aggregator | the bridged endpoint is listed |
| read `OnOff.OnOff` → `false`, `Toggle`, read → `true` | reads come from the engine's value map; a command reaches the product as `CommandInvoked`, the product answers and sets the value |
| read `NodeLabel` → `"Light"`, write `"Kitchen"`, read → `"Kitchen"` | a write reaches the product as `WriteRequested`; `Success` stores the value |
| write a label longer than the example allows → error, read → still `"Kitchen"` | `ConstraintError` from the product leaves the value unchanged |
| read `Reachable` → `true` | the Bridged Device Basic Information cluster is served |
| restart, read `OnOff` → `true`, `NodeLabel` → `"Kitchen"`, toggle, read `ConfigurationVersion` | the fabric survives in the state directory, no re-commissioning; the product restores its own state |

## Prerequisites

- The Rust toolchain of the workspace and Node.js (the shell is a Node application).
- A matter.js clone with the shell built: `packages/nodejs-shell/dist/esm/app.js` must exist.
  Building: `npm ci` in the clone, then `npx nacho-build`. On Windows without symlink
  privilege the `prepare` step of `npm ci` fails on a missing `dist/cjs` directory; create the
  directories first (`for p in packages/*; do mkdir -p $p/dist/cjs; done` in Git Bash) and run
  `npx nacho-build` again.
- UDP ports 5540 (Matter) and 5353 (mDNS) free for the example, and on Windows a firewall that
  lets the example binary (`target/debug/examples/onoff_node.exe`) receive on them. Windows asks
  on the first start; answer with allow, or create an inbound rule beforehand.
- An interface with an IPv4 address. The engine picks one itself; name it explicitly when the
  host has several (for example a virtual adapter of WSL or a VM).
- No other node of this project announcing on the host while the scenario runs: the contract
  tests of the engine crate (`cargo test`) start nodes with the same test discriminator on
  ephemeral ports, and the shell may pick one of them up during discovery. Run the scenario and
  the test suite one after the other, not at the same time.

## Running

Windows (PowerShell 5.1 or 7):

```powershell
./tests/interop-local/matterjs-shell.ps1 -MatterJs <path to the matter.js clone> -Interface Ethernet
```

Linux:

```sh
./tests/interop-local/matterjs-shell.sh <path to the matter.js clone> eth0
```

Both scripts build the example, run the two step files through `drive-shell.mjs`, print the
result of every step as JSON and end with `PASS` or `FAIL`. The work directory (default:
`matter-rs-interop-local` in the temporary directory) holds the node's state and output, the
controller's storage and the complete shell log. **The logs contain the pairing code**; they are
not committed.

## Files

| File | Role |
|---|---|
| `matterjs-shell.ps1`, `matterjs-shell.sh` | The scenario: build, start, commission, restart, verdict |
| `drive-shell.mjs` | Feeds the shell one command at a time and judges each step by regular expressions |
| `steps-commission.json` | The steps of the first run, from commissioning to the write that is refused |
| `steps-restart.json` | The steps after the restart |

A step has `cmd` (the shell command, `${PAIRING_CODE}` is substituted), `expect` (the pattern
that ends the step), `assert` (what must be in the step's output) and optionally `reject` (what
must not be) and `timeoutMs`.
