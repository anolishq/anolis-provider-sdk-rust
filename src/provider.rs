//! The trait a provider implements.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::model::{Device, DeviceHealth, Function, Health, HostRequirement, ProviderError, Readings, Results, Value};

/// The error a provider's own hooks (`new`, `start`, `check_config`,
/// `check_host`) return. A `String` or `&str` converts into it with `.into()`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The validated arguments of a call, keyed by argument name.
///
/// Every required argument is present and every value has its declared type
/// and lies within its declared bounds, so the typed getters return `None`
/// only for an optional argument the call left out (or a name the function
/// does not declare).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args(pub(crate) HashMap<String, Value>);

impl Args {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.0.get(name) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        match self.0.get(name) {
            Some(Value::Int64(i)) => Some(*i),
            _ => None,
        }
    }

    pub fn u64(&self, name: &str) -> Option<u64> {
        match self.0.get(name) {
            Some(Value::Uint64(u)) => Some(*u),
            _ => None,
        }
    }

    pub fn f64(&self, name: &str) -> Option<f64> {
        match self.0.get(name) {
            Some(Value::Double(d)) => Some(*d),
            _ => None,
        }
    }

    pub fn str(&self, name: &str) -> Option<&str> {
        match self.0.get(name) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        match self.0.get(name) {
            Some(Value::Bytes(b)) => Some(b),
            _ => None,
        }
    }
}

/// What a call knows besides its arguments.
#[derive(Clone, Debug, Default)]
pub struct CallContext {
    /// The client's deadline for the call (semantics.md §8.4), already checked
    /// to lie in the future when `call` is invoked.
    ///
    /// The SDK cannot interrupt a running call. A call that can take long
    /// should check [`CallContext::expired`] between steps and stop early: if
    /// the deadline passes before `call` returns, the SDK answers
    /// `CODE_DEADLINE_EXCEEDED` with `side_effects = possible` whatever the
    /// call returned, so what it did after the deadline is effort the client
    /// never sees as success.
    pub deadline: Option<SystemTime>,
}

impl CallContext {
    /// Time left before the deadline: `None` without one, zero once it has passed.
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| d.duration_since(SystemTime::now()).unwrap_or(Duration::ZERO))
    }

    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| SystemTime::now() >= d)
    }
}

/// A device provider. The SDK owns the protocol: framing, the Hello gate,
/// request dispatch, the default signal set, argument validation, deadlines,
/// timestamps, health and the command line. A provider owns its devices and
/// its driver.
///
/// `health` and `device_health` are answered on the request path, so return
/// state you already hold. Do not talk to hardware in them.
pub trait Provider: Sized + Send + 'static {
    /// The provider's config, read from the YAML or JSON file `--config` names.
    ///
    /// Its JSON Schema, derived with `#[derive(JsonSchema)]` unless
    /// [`Provider::config_schema`] is overridden, is what `--config-schema`
    /// prints and what every config is checked against before it is
    /// deserialized. Mark the struct `#[serde(deny_unknown_fields)]` so a
    /// misspelled key is refused rather than ignored.
    type Config: DeserializeOwned + JsonSchema + Clone + Send;

    /// The provider's name, reported in Hello and the CLI envelopes.
    const NAME: &'static str;
    /// The provider's version, reported in Hello and by `--version`.
    const VERSION: &'static str;
    /// The conformance level the provider claims, advertised in Hello.
    /// `None` advertises none, which a client reads as level 1.
    const CONFORMANCE_LEVEL: Option<u32> = None;

    /// Build the provider from a validated config. Keep this quick and leave
    /// the hardware alone: slow start-up belongs in [`Provider::start`]. An
    /// error ends the process with exit code 1.
    fn new(config: Self::Config) -> Result<Self, BoxError>;

    /// The JSON Schema (Draft 2020-12) for the config.
    fn config_schema() -> serde_json::Value {
        schemars::schema_for!(Self::Config).to_value()
    }

    /// Refuse a config the schema accepts but this provider would not (a
    /// duplicate device id, two devices on one address). Runs after schema
    /// validation for `--check-config`, `--check-host` and `--config`, so the
    /// three agree.
    fn check_config(_config: &Self::Config) -> Result<(), BoxError> {
        Ok(())
    }

    /// What this config needs from the host (executable profile §6). Read-only:
    /// inspect device nodes and parameters, do not talk to the hardware. Run
    /// for `--check-host`, and again at startup, where an unmet requirement
    /// keeps the provider up but not ready, with no devices.
    fn check_host(_config: &Self::Config) -> Result<Vec<HostRequirement>, BoxError> {
        Ok(Vec::new())
    }

    /// Called once, after Hello and before the first request after it. Put
    /// slow start-up here (opening a bus, probing devices) so Hello is
    /// answered at once. `budget` is the client's WaitReady hint, or zero.
    /// An error leaves the provider up but not ready, and reported in health.
    fn start(&mut self, _budget: Duration) -> Result<(), BoxError> {
        Ok(())
    }

    /// The devices this provider currently exposes.
    fn devices(&self) -> &[Device];

    /// A reading for every signal id asked for. The ids are declared signals
    /// of the device's type.
    fn read(&mut self, device: &Device, signal_ids: &[&str]) -> Result<Readings, ProviderError>;

    /// Carry out a validated call. Return the declared results, and nothing
    /// else; a function that declares none returns an empty map.
    fn call(
        &mut self,
        device: &Device,
        function: &Function,
        args: &Args,
        ctx: &CallContext,
    ) -> Result<Results, ProviderError>;

    fn health(&self) -> Health {
        Health::ok()
    }

    /// `None` lets the SDK report OK with the time of the last successful read.
    fn device_health(&self, _device: &Device) -> Option<DeviceHealth> {
        None
    }

    /// Called exactly once when the session ends, however it ends: stdin EOF,
    /// a framing or decode error, a closed stdout, or SIGTERM. Drive outputs
    /// to their safe state here: on hardware without a watchdog they otherwise
    /// hold their last value after the provider exits. It may run without
    /// `start` having run.
    fn stop(&mut self) {}
}
