# Configuration reference

<!-- Generated from the config types. Do not edit by hand: regenerate with
     `MATTER_RS_BLESS=1 cargo test -p matter-rs-config --all-features --test generated_docs`. -->

matter-rs is configured by one TOML file. `matter-rs init` writes a commented starter file,
`matter-rs validate` checks a file and reports every error and warning with its line and column,
and `matter-rs schema` prints the JSON schema of the file (also in
[`config.schema.json`](config.schema.json)).

The file is found in this order: the `--config <path>` option, the environment variable
`MATTER_RS_CONFIG`, then the platform default, `/etc/matter-rs/config.toml` on Linux and
`%ProgramData%\matter-rs\config.toml` on Windows.

Keys that are not listed here are errors, so that a mistyped key cannot be silently ignored.
Relative paths are relative to the directory of the configuration file.

**Secrets** such as the setup passcode accept three forms: the value itself, `{ env = "NAME" }`
to read it from an environment variable, or `{ file = "PATH" }` to read it from a file
(surrounding whitespace is ignored). `matter-rs validate` checks that the variable or file exists
but never prints a secret. On Unix, a secret written into a configuration file that other users
can read is reported as a warning.

## `[node]`

Identity and commissioning data of the Matter node.

This section is required.

| Key | Type | Default | Description |
|---|---|---|---|
| `vendor_id` | integer, 1 to 65535 | `65521` | Vendor id announced by the node. The default is the test vendor id `0xFFF1`. |
| `product_id` | integer, 1 to 65535 | `32769` | Product id announced by the node. |
| `vendor_name` | string, 1 to 32 characters | `"matter-rs"` | Vendor name shown by controllers. |
| `product_name` | string, 1 to 32 characters | `"matter-rs bridge"` | Product name shown by controllers. |
| `node_label` | string, at most 32 characters | `"matter-rs"` | User-visible label of the node. |
| `serial_number` | string, 1 to 32 characters | – | Serial number announced by the node, if any. |
| `passcode` | integer, at least 0, or a table with `env`, or a table with `file` | required | Setup passcode for commissioning: 1 to 99999998, without the trivial values the Matter specification forbids. A secret: write it here, or read it with `{ env = "NAME" }` or `{ file = "PATH" }`. |
| `discriminator` | integer, 0 to 4095 | required | Discriminator that commissioners use to find the node, 0 to 4095. |

## `[network]`

How the node is reached on the network.

This section is optional.

| Key | Type | Default | Description |
|---|---|---|---|
| `interface` | string, not empty | – | Network interface to use, by name. All interfaces when absent. |
| `ipv4` | boolean | `true` | Use IPv4. |
| `ipv6` | boolean | `true` | Use IPv6. |
| `port` | integer, 1 to 65535 | `5540` | UDP port of the Matter protocol. |

## `[state]`

Where the service keeps its state (fabrics, keys, counters).

This section is optional.

| Key | Type | Default | Description |
|---|---|---|---|
| `dir` | string | – | State directory. A relative path is relative to the directory of the configuration file. When absent: `/var/lib/matter-rs` for root and `$XDG_STATE_HOME/matter-rs` (or `~/.local/state/matter-rs`) otherwise on Linux; `%ProgramData%\matter-rs` when elevated and `%LOCALAPPDATA%\matter-rs` otherwise on Windows. |

## `[log]`

Logging.

This section is optional.

| Key | Type | Default | Description |
|---|---|---|---|
| `level` | one of `"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"` | `"info"` | Lowest level that is logged. |
| `format` | one of `"text"`, `"json"` | `"text"` | Format of log lines. |
| `journald` | boolean | `false` | Log to the systemd journal. Linux only; ignored with a warning elsewhere. |

## `[bridge]`

The bridge role. The section's presence enables the role. It needs a build with the Cargo feature `role-bridge`.

This section is optional.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | boolean | `true` | Run the bridge role. Set to `false` to switch the role off without removing the section. |

## `[adapters.<id>]`

Adapter instances, one table `[adapters.<id>]` per instance. `kind` selects the adapter; the other keys depend on the kind. A kind needs a build with the Cargo feature `adapter-<kind>`.

This section is optional.

No kind is available yet.
