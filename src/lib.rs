//! A Rust SDK for writing Anolis device providers.
//!
//! Declare each device type once as data, implement [`Provider`], and end the
//! binary with `fn main() -> ExitCode { anolis_provider_sdk::run::<MyProvider>() }`.
//! The SDK speaks ADPP v1 over framed stdio and implements the Anolis
//! executable profile's command line; the provider owns its devices and its
//! driver. See `docs/writing-a-provider.md`, and `examples/reference.rs` for a
//! complete provider.

mod cli;
mod framing;
mod model;
mod provider;
mod session;

/// The ADPP v1 messages, generated from the anolis-protocol schema vendored
/// under `proto/` (`just gen`).
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod proto {
    include!("proto/anolis.deviceprovider.v1.rs");
}

pub use cli::{ConfigError, config_schema, load_config, run};
pub use framing::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
pub use model::{
    Arg, Category, Device, DeviceHealth, DeviceState, DeviceType, Function, Health, HealthState, HostRequirement,
    HostStatus, ProviderError, Quality, Reading, Readings, Results, Signal, StatusCode, Value, ValueType,
};
pub use provider::{Args, BoxError, CallContext, Provider};
pub use session::{PROTOCOL_VERSION, Session, serve};
