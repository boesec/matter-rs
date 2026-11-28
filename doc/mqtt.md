# MQTT contract

How matter-rs looks on an MQTT broker and how it reads devices from one. The contract follows the
**mqtt-smarthome specification 2.0**
(<https://github.com/mqtt-smarthome/mqtt-smarthome/blob/master/SPEC.md>, version 2.0 of
2026-09-16). Section numbers below refer to that document. Every deliberate deviation from it is
listed in [Deviations from the specification](#deviations-from-the-specification) with its reason.

The MQTT adapter is configured in the one configuration file: an `[adapters.<id>]` table of kind
`mqtt` per broker, and one `[devices.<id>]` table per device read from it. The keys are listed in
the [configuration reference](config.md); this document says what they mean on the wire.

## 1. The bridge as an instance

Every MQTT adapter instance presents the bridge on its broker as one mqtt-smarthome instance with
the name `<name>` (`name` of the adapter table, default `matter`). It implements sections 3, 4, 5
and 6 of the specification and states `spec` `2.0` in its `info`.

| Topic | Retained | Payload | Meaning |
|---|---|---|---|
| `<name>/connected` | yes | plain `0`, `1` or `2` | `0`: not connected to the broker (set by the Last Will and published on a clean stop); `1`: connected, the Matter node is not running; `2`: connected and the node runs (section 3.1) |
| `<name>/info` | yes | JSON object | `name` `matter-rs`, `version` (the matter-rs version), `spec` `2.0`, `host`, `pid`, `started` (ISO 8601, UTC), `maintenance` `false`, `matter` `{ "vendor_id", "product_id" }` (section 6; `host` is left out when the host name is unknown) |
| `<name>/status/commissioning/commissioned` | yes | status object with a boolean | Whether the node is part of at least one fabric |
| `<name>/status/commissioning/window_open` | yes | status object with a boolean | Whether a commissioning window is open |
| `<name>/status/commissioning/fabric_count` | yes | status object with an integer | How many fabrics the node is part of |
| `<name>/status/devices/<id>/reachable` | yes | status object with a boolean | Whether the bridged device `<id>` (the `[devices.<id>]` table) is reachable, as the bridge reports it to controllers |

Status objects are `{ "val": ..., "ts": ..., "lc": ... }` (section 5.2) with `ts` and `lc` in
milliseconds since the epoch; `lc` keeps the time of the last change when a value is republished
unchanged. With `json_payloads = false` the same items carry plain values. `connected` is always
plain.

Everything is published with QoS 0 (section 4). On every broker connect the instance publishes
`connected`, `info`, the commissioning items and every device's `reachable`, so that the broker's
retained state is complete after a reconnect (section 3.2). On a clean stop it publishes
`connected` `0` before it disconnects; the Last Will publishes the same `0` when the connection
is lost.

**No pairing code and no QR text is ever published on MQTT.** The setup passcode, the manual
pairing code and the onboarding payload are secrets; `matter-rs pairing-info` is the one place
they are shown. The instance publishes the commissioning *state* only.

**No `set` topics.** The instance accepts no commands in this release; opening the commissioning
window from the broker is a security surface and is left to a later, opt-in improvement.

## 2. Devices read from the broker

A `[devices.<id>]` table with `adapter = "<mqtt adapter id>"` reads one device from the broker.
Its `mapping` says where the device lives; the mqtt-smarthome convention is the default, so a
device of another mqtt-smarthome adapter needs three lines:

```toml
[devices.lamp]
kind = "dimmable-light"
adapter = "home"
mapping = { source = "hue/lamp" }
```

`source` is `<adapter>/<device>`: the first two topic levels of the source, the instance name of
the adapter that publishes the device and the device's name on it. With it the device:

- reads attribute `x` from `<adapter>/status/<device>/<item>`,
- controls it through `<adapter>/set/<device>/<item>`,
- is reachable while `<adapter>/connected` is `2`,

where `<item>` is the attribute's snake_case name from the table below.

### 2.1 Attributes and their default items

| Attribute (`items.<name>`) | Default item | Matter attribute | Payload type and range | Device kinds | Mapped by default |
|---|---|---|---|---|---|
| `on-off` | `on_off` | On/Off `OnOff` | boolean | every light, the plug | yes |
| `level` | `level` | Level Control `CurrentLevel` | integer 1 to 254, or null | `dimmable-light`, `color-temperature-light`, `extended-color-light` | yes |
| `color-temperature` | `color_temperature` | Color Control `ColorTemperatureMireds` | integer 0 to 65279 (mireds) | `color-temperature-light`, `extended-color-light` | yes |
| `color-temperature-min` | `color_temperature_min` | Color Control `ColorTempPhysicalMinMireds` | integer 0 to 65279 | `color-temperature-light`, `extended-color-light` | **no**: give a topic or a fixed `value` |
| `color-temperature-max` | `color_temperature_max` | Color Control `ColorTempPhysicalMaxMireds` | integer 0 to 65279 | `color-temperature-light`, `extended-color-light` | **no**: give a topic or a fixed `value` |
| `hue` | `hue` | Color Control `CurrentHue` | integer 0 to 254 | `extended-color-light` | yes |
| `saturation` | `saturation` | Color Control `CurrentSaturation` | integer 0 to 254 | `extended-color-light` | yes |
| `color-x` | `color_x` | Color Control `CurrentX` | integer 0 to 65279 | `extended-color-light` | yes |
| `color-y` | `color_y` | Color Control `CurrentY` | integer 0 to 65279 | `extended-color-light` | yes |
| `state` | `state` | Boolean State `StateValue` | boolean (`true` is closed) | `contact-sensor` | yes |
| `temperature` | `temperature` | Temperature Measurement `MeasuredValue` | integer -27315 to 32767 (hundredths of a degree Celsius), or null | `temperature-sensor` | yes |
| `humidity` | `humidity` | Relative Humidity Measurement `MeasuredValue` | integer 0 to 10000 (hundredths of a percent), or null | `humidity-sensor` | yes |
| `pressure` | `pressure` | Pressure Measurement `MeasuredValue` | integer -32768 to 32767 (tenths of a kilopascal), or null | `pressure-sensor` | yes |
| `illuminance` | `illuminance` | Illuminance Measurement `MeasuredValue` | integer 0 to 65534 (`10000 * log10(lux) + 1`), or null | `light-sensor` | yes |
| `occupancy` | `occupancy` | Occupancy Sensing `Occupancy` | boolean or `0`/`1` | `occupancy-sensor` | yes |
| `name` | `name` | the device label (Bridged Device Basic Information `NodeLabel`) | string, `set` only | every kind | **no**: list `items.name` to publish renames |

The ranges and units are those of the Matter clusters; a source that publishes other units uses
a transform (section 2.3). The `generic-switch` kind is not supported by the MQTT adapter in this
release: its presses are events, not values.

**Composed devices** (`parts` instead of `kind`) map every part under `mapping.parts`, one entry
per part in the order of `parts`. The default item of attribute `x` of part `i` is `<i>/<x>`,
or `<prefix>/<x>` when the entry names a `prefix`. Only `name` may appear in the device's own
`items`.

### 2.2 What the adapter accepts

Status payloads are read tolerantly (section 5):

- A **plain value**: the text of a boolean, a number or a string.
- A **status object** `{ "val": ..., "ts": ..., "lc": ... }`: `val` is taken, `ts` and `lc` are
  ignored (the bridge keeps no timestamps), every other field is ignored.
- Booleans accept `true`/`false`, `1`/`0`, `on`/`off` and `yes`/`no`, case-insensitive, and the
  numbers `1` and `0` (section 5.3). Numbers accept numeric text and are rounded to the nearest
  integer and clamped to the attribute's range.
- An **empty payload** means "no value": an attribute that may be null (the measurements,
  `level`) becomes null, any other attribute keeps its last value.
- Anything the mapping cannot read (`bright` for a level, an array, a missing JSON pointer) is
  logged at debug level and ignored; the last good value stands. No payload makes the adapter
  fail.

### 2.3 Transforms

An item may name one of these in `items.<name>`; they turn the source's value into the
attribute's and back for a `set`:

| Key | Attribute type | Meaning |
|---|---|---|
| `item = "<name>"` | any | Another item name under the convention (`power` instead of `on_off`) |
| `status = "<topic>"`, `set = "<topic>"` | any | Full topics, replacing the convention's; either or both |
| `value = ...` | any | A fixed value: the attribute is never read from or written to the broker (the physical colour temperature bounds, say) |
| `path = "/state/on"` | any | A JSON pointer (RFC 6901) into the payload, instead of the `val` unwrapping; a `set` builds the nested object |
| `map = { "ON" = true, "OFF" = false }` | any | Payload text to attribute value, matched case-insensitively; a `set` publishes the key whose value matches |
| `factor = 100` | number | Attribute value = payload × factor (degrees Celsius to hundredths); a `set` divides |
| `scale = [0, 100]` | number | The payload range mapped linearly onto the attribute's range (a percentage to 1 to 254); a `set` maps back and rounds when the bounds are integers |
| `invert = true` | boolean, or number with `scale` | Negates a boolean; reverses the direction of a scale |

`map` excludes `factor` and `scale`; `factor` excludes `scale`; a `value` takes no transform.
The order inbound is `path` (or `val`), `map`, `factor` or `scale`, `invert`, then rounding and
clamping. A transform that does not fit the attribute's type (a `scale` on `on-off`) is a
configuration error reported at its key by `matter-rs validate`.

### 2.4 Reachability

A device is reachable (`Reachable` true for controllers, `<name>/status/devices/<id>/reachable`
true on the broker) while all of this holds:

- `<adapter>/connected` of its source is `2` (section 3.1); `0`, `1` and "never published" are
  unreachable;
- when the mapping names an `online` item, `<adapter>/status/<device>/<online>` is true;
- every attribute the device kind requires has received a value at least once (fixed values
  count). A device whose source has never published stays unreachable until the first value.

The device exists on the bridge from the start, so controllers see it at once. Until the first
value arrives, attributes that may be null are null and the others carry the cluster's "off"
value (`false`, `0`), with the device marked unreachable.

### 2.5 Commands become `set` messages

A command of a Matter controller becomes a publish to the item's `set` topic (section 3.3):

- plain value by default (`true`, `100`, `21.5`), `{ "val": ... }` with `set_json = true`;
- the inverse transform applied: a level of 254 under `scale = [0, 100]` is published as `100`;
- never retained, QoS 0;
- **never echoed as status.** The bridge already serves the target value to controllers; the
  source's own status message, when it comes, is what the adapter reads back.

| Matter command | Published items |
|---|---|
| On, Off, Toggle | `on-off` |
| MoveToLevel and friends | `level`, plus `on-off` for the `WithOnOff` variants |
| MoveToColorTemperature | `color-temperature` |
| MoveToHue, MoveToSaturation, MoveToHueAndSaturation | `hue`, `saturation` |
| MoveToColor | `color-x`, `color-y` |
| A write of `NodeLabel` (a rename) | `name`, only when `items.name` is listed; otherwise logged at debug level |
| A write of another writable attribute of the device (`StartUpOnOff`, `OnLevel`, ...) | nothing: the bridge keeps and serves the value itself (logged at debug level) |
| Identify, TriggerEffect | nothing (logged at debug level) |
| Commands and writes on extra clusters | answered `UNSUPPORTED_COMMAND` / `UNSUPPORTED_WRITE` |

While the broker connection is down nothing can be published; a `set` that falls into that
window is dropped with a log line, and the bridge keeps serving the target value.

## 3. Connection and recovery

- MQTT 3.1.1 over TCP (`mqtt://host[:port]`, port 1883 by default), clean session, the client
  id `client_id` (default `matter-rs-<adapter id>`), keep-alive `keep_alive` seconds (30), the
  Last Will `<name>/connected` = `0` retained.
- Credentials come from `username` and `password` (a secret: plain, `{ env = ... }` or
  `{ file = ... }`), or from `user:password@` in the URL. The URL is logged without them; the
  password appears in no log line, error message or report.
- When the connection is lost the adapter reports the health `degraded` to the service, keeps the
  bridged devices as they are (their last values and reachability stand) and reconnects with
  exponential backoff from `reconnect.initial` seconds (1) doubling up to `reconnect.max` (60).
  On reconnect it republishes `connected`, `info`, the commissioning items and every `reachable`,
  and subscribes to every mapped topic again; the retained status items of the sources then
  arrive anew.
- `mqtts://` and `tls_ca` are reserved: this build contains no TLS and refuses both with a
  message, because the MQTT client's TLS dependencies are under open security advisories.

## 4. Logging

Following section 11: the lifecycle (connecting, connected, subscribed, device added, device
reachable or unreachable, stopping) at `info`; a lost connection at `warn` once, with the delay
before the next attempt, and the following attempts at `debug`; raw traffic at `debug` with the
prefixes `mqtt >` (published) and `mqtt <` (received); a value the bridge refuses at `warn`; a
payload the mapping cannot read at `debug`.

## Deviations from the specification

| Specification | matter-rs | Reason |
|---|---|---|
| Section 7, maintenance topics (`maintenance/set/loglevel`, `maintenance/set/restart`, `maintenance/stats`) | not provided; `info.maintenance` is `false` | They let anyone on the broker restart the service or change its log level; the service is managed by systemd or a container and its log level by the configuration. A later improvement may add them behind an option that is off by default. |
| Section 8, the interface description (Home Assistant MQTT discovery) for the instance's own items | not published | The bridge's items (commissioning state, device reachability) are operational state of the bridge, not devices to control; a Home Assistant user controls the bridged devices through Matter itself. A later improvement may announce the instance. |
| Section 3.3, `set` topics on the instance | none | Opening the commissioning window over MQTT is a security surface on a bus that may not be authenticated; it waits for an opt-in improvement. |
| Section 9, CLI and environment conventions | none | matter-rs is configured by one file by design; the broker URL, credentials, instance name and payload form are keys of the adapter table, not command-line options or environment variables. |
| Section 10, device discovery | none | The broker is named in the configuration; the sources are other adapters. |
| Section 11, journald output format | the service's own | The service logs through its `[log]` section (text, JSON or the journal); the prefixes `mqtt >` and `mqtt <` are followed. |
| Section 3.2, clearing items of a previous run | `reachable` items of devices removed from the configuration stay retained until the broker forgets them | Devices are static in this release (no hot reload); an improvement that removes devices at run time clears their items. |
| Section 5.2, `ts` and `lc` of the sources | ignored | The bridge keeps no timestamps for attribute values. |
