# Writing a provider

A provider is an executable that the Anolis runtime starts with
`--config <path>` and talks to over stdin and stdout. This SDK speaks the
protocol; a provider declares its devices and implements three methods.
[`examples/reference.rs`](../examples/reference.rs) is a complete provider to
copy from.

## 1. Declare the device types

A device type is data: its signals, its functions and their arguments. The
SDK derives everything the protocol needs from it: function ids (1, 2, … in
declaration order), the default signal set, and the checks every call's
arguments go through before your code sees them.

```rust
use std::sync::{Arc, LazyLock};
use anolis_provider_sdk::{Arg, Category, DeviceType, Function, Signal, ValueType};

static HEATER: LazyLock<Arc<DeviceType>> = LazyLock::new(|| {
    DeviceType::new("heater")
        .signal(Signal::new("temperature_c", ValueType::Double).unit("C").stale_after_ms(5000))
        .signal(Signal::new("power_w", ValueType::Double).unit("W"))
        .signal(Signal::new("serial", ValueType::String).not_default().poll_hint_hz(0.0))
        .function(
            Function::new("set_power")
                .arg(Arg::double("percent").range(0.0, 100.0).unit("%"))
                .category(Category::Actuate),
        )
        .function(
            Function::new("self_test")
                .result(Arg::bool("passed"))
                .category(Category::Read),
        )
        .build()
});
```

- **Signals** are default signals unless marked `not_default()`: a read that
  names no signals returns the defaults. The runtime polls a signal only when
  its `poll_hint_hz` is above 0 (1.0 unless set), so set 0 on a signal that is
  only read on demand.
- **Arguments** are required unless marked `optional()`. Bounds are
  inclusive. A call with a missing or mistyped argument is refused with
  `CODE_INVALID_ARGUMENT`, and one outside a bound with `CODE_OUT_OF_RANGE`,
  before your code runs.
- **Results** are declared too. A completed call must return exactly the
  declared results; one missing or one extra is a bug the SDK reports as
  `CODE_INTERNAL`.
- **Category** tells the runtime what a call does: `Actuate` (the default)
  for anything that moves or powers something, `Config` for settings, `Read`
  for calls without effects.

A declaration that breaks a protocol rule (a signal id that is not
snake_case, a bound on a string argument, two functions with one name, a type
whose signals are all `not_default()`) panics at the line that declares it.

## 2. Type the config

The config is a struct. Its JSON Schema is derived from it, printed by
`--config-schema`, and checked against every config before it is
deserialized, so the schema a client sees and the config the provider accepts
cannot disagree.

```rust
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    bus: String,
    #[serde(default)]
    devices: Vec<DeviceConfig>,
}
```

Mark every struct `deny_unknown_fields`, so a misspelled key is refused rather
than ignored. Use `#[schemars(...)]` attributes for what the schema should say
beyond the types: a pattern, a range. What a schema cannot express (two devices
on one address) goes in `check_config`.

## 3. Implement Provider

```rust
use anolis_provider_sdk::*;

struct Heaters { devices: Vec<Device> /* and the driver */ }

impl Provider for Heaters {
    type Config = Config;
    const NAME: &'static str = "anolis-provider-heaters";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const CONFORMANCE_LEVEL: Option<u32> = Some(2);

    fn new(config: Config) -> Result<Self, BoxError> {
        // Quick, and no hardware: slow start-up belongs in start().
        todo!()
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn read(&mut self, device: &Device, signal_ids: &[&str]) -> Result<Readings, ProviderError> {
        // A value for every id asked for. A plain value is stamped with the
        // time of the read and quality OK; a Reading carries its own.
        todo!()
    }

    fn call(&mut self, device: &Device, function: &Function, args: &Args, ctx: &CallContext)
    -> Result<Results, ProviderError> {
        match function.name() {
            "set_power" => {
                let percent = args.f64("percent").expect("required, so present");
                // drive the hardware
                Ok(Results::new())
            }
            _ => unreachable!("the SDK only passes declared functions"),
        }
    }

    fn stop(&mut self) {
        // Drive every output to its safe state.
    }
}

fn main() -> std::process::ExitCode {
    anolis_provider_sdk::run::<Heaters>()
}
```

Return a `ProviderError` to answer with a specific status:
`ProviderError::unavailable("bus timed out")` for a device that did not answer,
`ProviderError::failed_precondition(...)` for one that cannot do this now.
Details go in `Status.details` with `.detail(key, value)`.

The other hooks have defaults:

| Hook | When it runs | Default |
| --- | --- | --- |
| `start(budget)` | Once, after Hello, before the next request | Nothing |
| `check_config(&config)` | After schema validation, for every verb that reads a config | Accept |
| `check_host(&config)` | For `--check-host`, and again at startup | No requirements |
| `health()` | On `GetHealth` | OK |
| `device_health(device)` | On `GetHealth` and `ListDevices` | OK, with the last read's time |
| `stop()` | Exactly once, however the session ends | Nothing |

`health` and `device_health` are answered on the request path: return state
you already hold, and do not talk to the hardware in them.

## Deadlines

A call can carry a deadline. The SDK refuses one that has already passed
before your code runs, and passes the rest to `call` in `ctx.deadline`. The
SDK cannot interrupt a running call, so a call that can take long should check
`ctx.expired()` between steps and stop early. If the deadline passes before
`call` returns, the client gets `CODE_DEADLINE_EXCEEDED` with
`side_effects = possible`, whatever the call returned.

## Host requirements

`check_host` reports what a config needs from the host: a device node present
and accessible, a bus parameter within a limit. It is read-only: inspect, do
not talk to the hardware.

```rust
fn check_host(config: &Config) -> Result<Vec<HostRequirement>, BoxError> {
    let node = std::path::Path::new(&config.bus);
    Ok(vec![if node.exists() {
        HostRequirement::met("bus.present")
    } else {
        HostRequirement::unmet("bus.present")
            .detail(format!("{} does not exist", node.display()))
            .remedy("enable the bus in the boot config")
    }])
}
```

At startup the SDK runs the same checks. On an unmet requirement the provider
stays up, not ready and with no devices, and reports what is unmet, so the
runtime can show why.

## Run the conformance harness

The anolis-protocol harness is the oracle: a provider is conformant when it
passes. Copy `config/conformance.toml`, `config/conformance.yaml` and
`scripts/conformance.py` from this repository, point them at your provider,
and run:

```bash
python3 -m venv .harness
.harness/bin/pip install "anolis-protocol[conformance] @ https://github.com/anolishq/anolis-protocol/releases/download/v1.10.0/anolis_protocol-1.10.0-py3-none-any.whl"
cargo build --release
python3 scripts/conformance.py target/release/<your-provider> .harness/bin/anolis-adpp-conformance
```

The harness cannot see everything. It checks the status a call returns, not
whether the call moved anything, so test the behaviour of your own functions
in your own tests.
