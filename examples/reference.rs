//! The reference provider: a synthetic `counter` device and no hardware.
//!
//! It is the SDK's own conformance fixture and a worked example. Its device
//! type is the Python SDK's reference counter, chosen so that every generic
//! test in the anolis-protocol harness has something to exercise: bounded
//! double, int64 and uint64 arguments, a function with a declared result, a
//! function with no arguments, and default and non-default signals.

use std::collections::HashSet;
use std::process::ExitCode;
use std::sync::{Arc, LazyLock};

use anolis_provider_sdk::{
    Arg, Args, BoxError, CallContext, Category, Device, DeviceType, Function, Provider, ProviderError, Readings,
    Results, Signal, Value, ValueType,
};
use schemars::JsonSchema;
use serde::Deserialize;

static COUNTER: LazyLock<Arc<DeviceType>> = LazyLock::new(|| {
    DeviceType::new("counter")
        .signal(Signal::new("count", ValueType::Int64).description("Current count"))
        .signal(Signal::new("step", ValueType::Int64).description("Amount added by increment"))
        .signal(Signal::new("rate", ValueType::Double).unit("1/s").description("Nominal rate"))
        .signal(Signal::new("enabled", ValueType::Bool).description("Whether increment changes the count"))
        .signal(Signal::new("limit", ValueType::Uint64).description("Largest allowed count").not_default())
        .signal(Signal::new("name", ValueType::String).description("Device label").not_default().poll_hint_hz(0.0))
        .function(Function::new("reset").description("Set the count to zero").category(Category::Config))
        .function(
            Function::new("increment")
                .description("Add step to the count when enabled")
                .result(Arg::int64("count").description("The count after the call"))
                .category(Category::Config),
        )
        .function(
            Function::new("set_step")
                .arg(Arg::int64("step").range(-1000, 1000).description("Amount per increment"))
                .category(Category::Config),
        )
        .function(
            Function::new("set_rate")
                .arg(Arg::double("rate").range(0.0, 100.0).unit("1/s").description("Nominal rate"))
                .category(Category::Config),
        )
        .function(
            Function::new("set_limit")
                .arg(Arg::uint64("limit").range(1, 1_000_000).description("Largest allowed count"))
                .category(Category::Config),
        )
        // CONFIG, not ACTUATE: the runtime's zero_is_safe rung can only drive an
        // ACTUATE function through a required numeric argument, and this one has none.
        .function(Function::new("set_enabled").arg(Arg::bool("enabled")).category(Category::Config))
        .build()
});

/// anolis-provider-reference config
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    devices: Vec<DeviceConfig>,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeviceConfig {
    #[schemars(regex(pattern = r"^[a-z][a-z0-9_]*$"))]
    id: String,
    label: Option<String>,
    #[serde(default)]
    start: u32,
}

struct Counter {
    count: i64,
    step: i64,
    rate: f64,
    enabled: bool,
    limit: u64,
}

struct ReferenceProvider {
    devices: Vec<Device>,
    counters: Vec<Counter>,
}

impl ReferenceProvider {
    fn counter(&mut self, device: &Device) -> &mut Counter {
        let index = self.devices.iter().position(|d| d.id() == device.id()).expect("the SDK passes known devices");
        &mut self.counters[index]
    }
}

impl Provider for ReferenceProvider {
    type Config = Config;
    const NAME: &'static str = "anolis-provider-reference";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const CONFORMANCE_LEVEL: Option<u32> = Some(2);

    fn check_config(config: &Config) -> Result<(), BoxError> {
        let mut seen = HashSet::new();
        for device in &config.devices {
            if !seen.insert(&device.id) {
                return Err(format!("device id {:?} is configured twice", device.id).into());
            }
        }
        Ok(())
    }

    fn new(config: Config) -> Result<Self, BoxError> {
        let devices = config
            .devices
            .iter()
            .map(|d| Device::new(&d.id, &COUNTER).label(d.label.clone().unwrap_or_else(|| d.id.clone())))
            .collect();
        let counters = config
            .devices
            .iter()
            .map(|d| Counter { count: d.start.into(), step: 1, rate: 1.0, enabled: true, limit: 1_000_000 })
            .collect();
        Ok(Self { devices, counters })
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn read(&mut self, device: &Device, signal_ids: &[&str]) -> Result<Readings, ProviderError> {
        let label = device.label_text().to_owned();
        let c = self.counter(device);
        let value = |id: &str| -> Value {
            match id {
                "count" => c.count.into(),
                "step" => c.step.into(),
                "rate" => c.rate.into(),
                "enabled" => c.enabled.into(),
                "limit" => c.limit.into(),
                _ => label.clone().into(),
            }
        };
        Ok(signal_ids.iter().map(|&id| (id.to_owned(), value(id).into())).collect())
    }

    fn call(
        &mut self,
        device: &Device,
        function: &Function,
        args: &Args,
        _ctx: &CallContext,
    ) -> Result<Results, ProviderError> {
        let c = self.counter(device);
        let mut results = Results::new();
        match function.name() {
            "reset" => c.count = 0,
            "increment" => {
                if c.enabled {
                    let limit = i64::try_from(c.limit).unwrap_or(i64::MAX);
                    c.count = c.count.saturating_add(c.step).clamp(-limit, limit);
                }
                results.insert("count".to_owned(), c.count.into());
            }
            "set_step" => c.step = args.i64("step").expect("required"),
            "set_rate" => c.rate = args.f64("rate").expect("required"),
            "set_limit" => c.limit = args.u64("limit").expect("required"),
            "set_enabled" => c.enabled = args.bool("enabled").expect("required"),
            other => unreachable!("undeclared function {other}"),
        }
        Ok(results)
    }
}

fn main() -> ExitCode {
    anolis_provider_sdk::run::<ReferenceProvider>()
}
