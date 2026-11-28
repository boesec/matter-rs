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

### `adapters` of kind `"mqtt"`

An MQTT broker; see the MQTT contract in `doc/mqtt.md`.

| Key | Type | Default | Description |
|---|---|---|---|
| `kind` | string | required |  |
| `url` | string, not empty | required | Broker URL: `mqtt://host[:port]` (port 1883 when absent). `mqtts://` is reserved for a TLS build. The URL may carry `user:password@`, but the `username` and `password` keys are the recommended place for credentials and win over the URL. |
| `username` | string, not empty | – | User name for the broker. |
| `password` | string, or a table with `env`, or a table with `file` | – | Password for the broker. A secret: write it here, or read it with `{ env = "NAME" }` or `{ file = "PATH" }`. |
| `client_id` | string, not empty | – | MQTT client id. Default `matter-rs-<id>` with the id of this adapter table. |
| `name` | string, not empty | `"matter"` | The instance name of the bridge on this broker: the first topic level of its own items, `<name>/connected`, `<name>/info`, `<name>/status/...`. Must not contain `/`, `+` or `#`. |
| `tls_ca` | string | – | CA certificate file (PEM) for `mqtts://` with a private CA. Reserved for a TLS build. |
| `json_payloads` | boolean | `true` | Publish the bridge's status items as `{ "val": ..., "ts": ..., "lc": ... }` objects (the mqtt-smarthome default). `false` publishes plain values. |
| `set_json` | boolean | `false` | Publish `set` messages for the devices as `{ "val": ... }` objects instead of plain values. |
| `keep_alive` | integer, 0 to 65535 | `30` | Seconds between keep-alive pings when nothing else is sent; `0` switches them off. |
| `reconnect` | a table | – | How the adapter reconnects after the broker connection was lost. |

#### `adapters.reconnect`

Reconnect delays after a lost broker connection: the delay starts at `initial`, doubles with every failed attempt and never exceeds `max`. A successful connection resets it.

| Key | Type | Default | Description |
|---|---|---|---|
| `initial` | number | `1.0` | The delay before the first reconnect attempt, in seconds. |
| `max` | number | `60.0` | The longest delay between two attempts, in seconds. |


## `[devices.<id>]`

Bridged devices, one table `[devices.<id>]` per device. The id is the device's stable key; every device names the adapter that feeds it and, in `mapping`, where the adapter finds it. Needs the bridge role.

This section is optional.

| Key | Type | Default | Description |
|---|---|---|---|
| `kind` | string, not empty | – | The device kind: `on-off-light`, `dimmable-light`, `color-temperature-light`, `extended-color-light`, `on-off-plug-in-unit`, `contact-sensor`, `temperature-sensor`, `humidity-sensor`, `pressure-sensor`, `light-sensor`, `occupancy-sensor` or `generic-switch`. A composed device lists its kinds in `parts` instead. |
| `adapter` | string, not empty | required | The id of the `[adapters.<id>]` table that feeds the device. |
| `name` | string, 1 to 32 characters | – | The label controllers show; the table's id when absent. |
| `parts` | array | – | The parts of a composed device, in order, each a kind with an optional label (a power strip with two sockets and a sensor). Replaces `kind`. |
| `mapping` | a table | required | Where the adapter finds the device. The keys depend on the adapter's kind and are listed per kind in the reference. |

### `mapping` for adapter kind `"mqtt"`

Where the device lives on the broker: its `source`, the two leading topic levels `<adapter>/<device>` of an mqtt-smarthome source, and the deviations from the convention.

| Key | Type | Default | Description |
|---|---|---|---|
| `source` | string, at least 3 characters | required | The source adapter instance and the device's name on it, `<adapter>/<device>`: the device reads `<adapter>/status/<device>/<item>` and writes `<adapter>/set/<device>/<item>`, and is reachable while `<adapter>/connected` is `2`. |
| `online` | string, not empty | – | The item that reports whether the device itself is reachable (`<adapter>/status/<device>/<online>`, a boolean); the device is unreachable while it is false. Not read when absent. |
| `items` | a table | – | Deviations per attribute, by the attribute's name (`on-off`, `level`, `temperature`, ..., and `name` for the device label). For a composed device only `name` may appear here; its parts map their attributes in `parts`. |
| `parts` | array | – | For a composed device: one entry per part, in the order of the device's `parts`. The default item of attribute `x` of part `i` is `<i>/<x>`, or `<prefix>/<x>`. |

#### `mapping.items.<name>`

How one attribute deviates from the convention: another item name or topic, a fixed value, or a transform (see the transforms table of the contract).

| Key | Type | Default | Description |
|---|---|---|---|
| `item` | string, not empty | – | The item name under the convention, replacing the attribute's `snake_case` name. |
| `status` | string, not empty | – | The full status topic, replacing `<adapter>/status/<device>/<item>`. |
| `set` | string, not empty | – | The full `set` topic, replacing `<adapter>/set/<device>/<item>`. |
| `value` | a value | – | A fixed value instead of a topic; the attribute is never read from or written to the broker. |
| `scale` | array | – | The payload range `[min, max]`, mapped linearly onto the attribute's range. |
| `invert` | boolean | `false` | Negate a boolean, or reverse the direction of `scale`. |
| `map` | a table | – | Payload text to attribute value, for example `{ "ON" = true, "OFF" = false }`; matched case-insensitively, the key is published for a `set`. |
| `path` | string | – | A JSON pointer into the payload (`/state/on`), instead of the `val` unwrapping. |
| `factor` | number | – | Attribute value = payload × factor (`100` turns degrees into hundredths). |

#### `mapping.parts[]`

The mapping of one part of a composed device.

| Key | Type | Default | Description |
|---|---|---|---|
| `prefix` | string, not empty | – | The item level of this part's attributes; the part's index when absent. |
| `items` | a table | – | Deviations per attribute of this part, as in the device's `items`. |
