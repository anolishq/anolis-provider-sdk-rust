//! One ADPP session: a request in, a response out (anolis-protocol
//! `docs/semantics.md`).
//!
//! [`Session::handle`] holds every protocol rule and does no I/O, so it can be
//! tested request by request. [`serve`] runs it over framed stdio.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prost::Message;

use crate::framing::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
use crate::model::{Device, DeviceState, HealthState, HostStatus, ProviderError, Quality, StatusCode};
use crate::proto::{self, request::Payload as Req, response::Payload as Resp};
use crate::provider::{Args, CallContext, Provider};

/// The ADPP version this SDK speaks.
pub const PROTOCOL_VERSION: &str = "v1";
const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The valid `google.protobuf.Timestamp` range, 0001-01-01 to 9999-12-31.
const TS_MIN_SECONDS: i64 = -62_135_596_800;
const TS_MAX_SECONDS: i64 = 253_402_300_799;

/// Quote client-supplied text for an error message without echoing it unbounded.
fn quote(text: &str) -> String {
    const LIMIT: usize = 64;
    match text.char_indices().nth(LIMIT) {
        Some((cut, _)) => format!("{:?}...", &text[..cut]),
        None => format!("{text:?}"),
    }
}

fn truncate(mut text: String, limit: usize) -> String {
    if let Some((cut, _)) = text.char_indices().nth(limit) {
        text.truncate(cut);
    }
    text
}

fn fail(code: StatusCode, message: impl Into<String>) -> ProviderError {
    ProviderError::new(code, message)
}

fn timestamp(at: SystemTime) -> prost_types::Timestamp {
    let (seconds, nanos) = match at.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_nanos() as i32),
        Err(before) => {
            let d = before.duration();
            let (s, n) = (d.as_secs() as i64, d.subsec_nanos() as i32);
            if n == 0 { (-s, 0) } else { (-s - 1, 1_000_000_000 - n) }
        }
    };
    prost_types::Timestamp { seconds, nanos }
}

fn system_time(ts: &prost_types::Timestamp) -> Result<SystemTime, ProviderError> {
    if !(0..=999_999_999).contains(&ts.nanos) || !(TS_MIN_SECONDS..=TS_MAX_SECONDS).contains(&ts.seconds) {
        return Err(fail(StatusCode::InvalidArgument, "deadline is not a valid timestamp"));
    }
    let nanos = Duration::from_nanos(ts.nanos as u64);
    Ok(if ts.seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(ts.seconds as u64) + nanos
    } else {
        UNIX_EPOCH - Duration::from_secs(ts.seconds.unsigned_abs()) + nanos
    })
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Executable profile §6: the `--check-host` checks, run again at startup.
enum HostCheck {
    /// The checks could not run, which, like an `unknown` requirement, does not block start.
    NotRun,
    Ok,
    Unmet(String),
}

/// One provider's protocol state.
pub struct Session<P: Provider> {
    provider: P,
    config: P::Config,
    hello_done: bool,
    started: bool,
    start_error: Option<String>,
    host_check: HostCheck,
    init_ms: u128,
    last_seen: HashMap<String, SystemTime>,
}

impl<P: Provider> Session<P> {
    /// `config` is the config `provider` was built from; the session runs the
    /// host checks against it at startup.
    pub fn new(provider: P, config: P::Config) -> Self {
        Self {
            provider,
            config,
            hello_done: false,
            started: false,
            start_error: None,
            host_check: HostCheck::NotRun,
            init_ms: 0,
            last_seen: HashMap::new(),
        }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    /// Answer one request. Never fails: an error becomes the response's status.
    pub fn handle(&mut self, request: proto::Request) -> proto::Response {
        let request_id = request.request_id;
        let result = match request.payload {
            None => Err(fail(StatusCode::InvalidArgument, "request carries no payload")),
            Some(Req::Hello(hello)) => self.hello(&hello),
            // semantics.md §3.2: nothing is processed before a successful Hello.
            Some(other) if !self.hello_done => {
                Err(fail(StatusCode::FailedPrecondition, format!("{} before Hello; send Hello first", kind(&other))))
            }
            Some(other) => {
                let name = kind(&other);
                // A panic in the provider is a bug in it; it must not take the session down.
                catch_unwind(AssertUnwindSafe(|| self.dispatch(other))).unwrap_or_else(|payload| {
                    let message = panic_message(payload.as_ref());
                    eprintln!("{}: ERROR {name} panicked: {message}", P::NAME);
                    Err(fail(StatusCode::Internal, truncate(format!("{name} panicked: {message}"), 512)))
                })
            }
        };
        match result {
            Ok(payload) => proto::Response {
                request_id,
                status: Some(proto::Status { code: proto::status::Code::Ok as i32, ..Default::default() }),
                payload: Some(payload),
            },
            Err(err) => proto::Response {
                request_id,
                status: Some(proto::Status {
                    code: err.code.to_proto() as i32,
                    message: err.message,
                    details: err.details,
                }),
                payload: None,
            },
        }
    }

    fn dispatch(&mut self, request: Req) -> Result<Resp, ProviderError> {
        let budget = match &request {
            Req::WaitReady(w) => Duration::from_millis(u64::from(w.max_wait_ms_hint)),
            _ => Duration::ZERO,
        };
        self.ensure_started(budget);
        match request {
            Req::Hello(_) => unreachable!("Hello is answered before dispatch"),
            Req::WaitReady(_) => Ok(Resp::WaitReady(self.wait_ready())),
            Req::ListDevices(r) => Ok(Resp::ListDevices(self.list_devices(&r))),
            Req::DescribeDevice(r) => self.describe_device(&r).map(Resp::DescribeDevice),
            Req::ReadSignals(r) => self.read_signals(&r).map(Resp::ReadSignals),
            Req::Call(r) => self.call(&r).map(Resp::Call),
            Req::GetHealth(_) => Ok(Resp::GetHealth(self.get_health())),
        }
    }

    fn ensure_started(&mut self, budget: Duration) {
        if self.started {
            return;
        }
        self.started = true;
        let began = Instant::now();
        self.run_host_check();
        if let HostCheck::Unmet(unmet) = &self.host_check {
            // §6: stay up and not ready, exposing no devices, rather than exit.
            eprintln!("{}: ERROR host requirements unmet: {unmet}", P::NAME);
        } else if let Err(err) = self.provider.start(budget) {
            eprintln!("{}: ERROR provider start failed: {err}", P::NAME);
            self.start_error = Some(truncate(err.to_string(), 512));
        }
        self.init_ms = began.elapsed().as_millis();
    }

    fn run_host_check(&mut self) {
        self.host_check = match P::check_host(&self.config) {
            Err(err) => {
                eprintln!("{}: ERROR host check could not run: {err}", P::NAME);
                HostCheck::NotRun
            }
            Ok(requirements) => {
                let unmet: Vec<String> = requirements
                    .iter()
                    .filter(|r| r.status == HostStatus::Unmet)
                    .map(|r| if r.detail.is_empty() { r.id.clone() } else { format!("{}: {}", r.id, r.detail) })
                    .collect();
                if unmet.is_empty() { HostCheck::Ok } else { HostCheck::Unmet(truncate(unmet.join("; "), 512)) }
            }
        };
    }

    fn devices(&self) -> &[Device] {
        if matches!(self.host_check, HostCheck::Unmet(_)) { &[] } else { self.provider.devices() }
    }

    fn device(&self, device_id: &str) -> Result<Device, ProviderError> {
        self.devices()
            .iter()
            .find(|d| d.id() == device_id)
            .cloned()
            .ok_or_else(|| fail(StatusCode::NotFound, format!("unknown device_id {}", quote(device_id))))
    }

    // ---- handshake --------------------------------------------------------

    fn hello(&mut self, request: &proto::HelloRequest) -> Result<Resp, ProviderError> {
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(fail(
                StatusCode::FailedPrecondition,
                format!(
                    "protocol_version {} is not supported; this provider speaks {PROTOCOL_VERSION}",
                    quote(&request.protocol_version)
                ),
            ));
        }
        let mut metadata = HashMap::from([
            ("transport".to_owned(), "stdio+uint32_le".to_owned()),
            ("max_frame_bytes".to_owned(), MAX_FRAME_BYTES.to_string()),
            ("supports_wait_ready".to_owned(), "true".to_owned()),
            ("supports_deadlines".to_owned(), "true".to_owned()),
        ]);
        if let Some(level) = P::CONFORMANCE_LEVEL {
            metadata.insert("conformance_level".to_owned(), level.to_string());
        }
        self.hello_done = true;
        Ok(Resp::Hello(proto::HelloResponse {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            provider_name: P::NAME.to_owned(),
            provider_version: P::VERSION.to_owned(),
            metadata,
        }))
    }

    fn wait_ready(&self) -> proto::WaitReadyResponse {
        let unmet = match &self.host_check {
            HostCheck::Unmet(unmet) => Some(unmet.as_str()),
            _ => None,
        };
        let ready = self.start_error.is_none() && unmet.is_none();
        let mut d = HashMap::from([
            ("init_time_ms".to_owned(), self.init_ms.to_string()),
            ("ready".to_owned(), ready.to_string()),
            ("device_count".to_owned(), self.devices().len().to_string()),
            (
                "provider_impl".to_owned(),
                format!("{} {} (anolis-provider-sdk-rust {SDK_VERSION})", P::NAME, P::VERSION),
            ),
        ]);
        match &self.host_check {
            HostCheck::NotRun => {}
            HostCheck::Ok => {
                d.insert("host_check".to_owned(), "ok".to_owned());
            }
            HostCheck::Unmet(unmet) => {
                d.insert("host_check".to_owned(), "unmet".to_owned());
                d.insert("host_unmet".to_owned(), unmet.clone());
            }
        }
        if let Some(err) = &self.start_error {
            d.insert("start_error".to_owned(), err.clone());
        }
        proto::WaitReadyResponse { diagnostics: d }
    }

    // ---- inventory --------------------------------------------------------

    fn list_devices(&self, request: &proto::ListDevicesRequest) -> proto::ListDevicesResponse {
        let devices = self.devices();
        proto::ListDevicesResponse {
            devices: devices.iter().map(|d| d.to_proto(P::NAME)).collect(),
            device_health: if request.include_health {
                devices.iter().map(|d| self.device_health(d)).collect()
            } else {
                Vec::new()
            },
        }
    }

    fn describe_device(
        &self,
        request: &proto::DescribeDeviceRequest,
    ) -> Result<proto::DescribeDeviceResponse, ProviderError> {
        let device = self.device(&request.device_id)?;
        Ok(proto::DescribeDeviceResponse {
            device: Some(device.to_proto(P::NAME)),
            capabilities: Some(device.device_type().capabilities()),
        })
    }

    // ---- reads ------------------------------------------------------------

    fn read_signals(
        &mut self,
        request: &proto::ReadSignalsRequest,
    ) -> Result<proto::ReadSignalsResponse, ProviderError> {
        let device = self.device(&request.device_id)?;
        let dtype = device.device_type();
        let signal_ids: Vec<&str> = if request.signal_ids.is_empty() {
            dtype.default_signal_ids()
        } else {
            request.signal_ids.iter().map(String::as_str).collect()
        };
        // semantics.md §7.4: one consistent behaviour, NOT_FOUND.
        if let Some(unknown) = signal_ids.iter().find(|id| dtype.signal_by_id(id).is_none()) {
            return Err(fail(
                StatusCode::NotFound,
                format!("unknown signal_id {} on device {}", quote(unknown), quote(device.id())),
            ));
        }
        let mut out = proto::ReadSignalsResponse { device_id: device.id().to_owned(), values: Vec::new() };
        if signal_ids.is_empty() {
            return Ok(out);
        }
        let mut readings = self.provider.read(&device, &signal_ids)?;
        let now = SystemTime::now();
        for id in &signal_ids {
            let signal = dtype.signal_by_id(id).expect("checked above");
            let reading = readings
                .remove(*id)
                .ok_or_else(|| fail(StatusCode::Internal, format!("provider returned no value for signal {id:?}")))?;
            let value = reading
                .value
                .coerce(signal.value_type())
                .map_err(|err| fail(StatusCode::Internal, format!("signal {id:?}: {err}")))?;
            let observed = reading.observed_at.unwrap_or(now);
            let mut quality = reading.quality;
            let age = now.duration_since(observed).unwrap_or(Duration::ZERO);
            if quality == Quality::Ok
                && signal.stale_after() > 0
                && age > Duration::from_millis(signal.stale_after().into())
            {
                quality = Quality::Stale;
            }
            out.values.push(proto::SignalValue {
                signal_id: (*id).to_owned(),
                value: Some(value.to_proto()),
                timestamp: Some(timestamp(observed)),
                quality: quality.to_proto() as i32,
                metadata: HashMap::new(),
            });
        }
        self.last_seen.insert(device.id().to_owned(), now);
        Ok(out)
    }

    // ---- calls ------------------------------------------------------------

    fn call(&mut self, request: &proto::CallRequest) -> Result<proto::CallResponse, ProviderError> {
        let device = self.device(&request.device_id)?;
        let function = resolve_function(&device, request)?.clone();
        let args = validate_args(&function, &request.args)?;
        let deadline = request.deadline.as_ref().map(system_time).transpose()?;
        if deadline.is_some_and(|d| SystemTime::now() >= d) {
            return Err(fail(
                StatusCode::DeadlineExceeded,
                "deadline expired before the call started; nothing was done",
            ));
        }
        let ctx = CallContext { deadline };
        let outcome = self.provider.call(&device, &function, &args, &ctx);
        // §8.4: a deadline that passed during the call is DEADLINE_EXCEEDED,
        // whatever the call returned. It ran, so its effects may have happened.
        if ctx.expired() {
            let mut err = fail(
                StatusCode::DeadlineExceeded,
                "deadline expired while the call ran; its effects may have happened",
            )
            .detail("side_effects", "possible");
            if let Err(call_err) = &outcome {
                err = err.detail("call_error", truncate(call_err.message.clone(), 512));
            }
            return Err(err);
        }
        let mut results = outcome?;
        let mut out = proto::CallResponse { device_id: device.id().to_owned(), ..Default::default() };
        for spec in function.results() {
            let value = results.remove(spec.name()).ok_or_else(|| {
                fail(
                    StatusCode::Internal,
                    format!("{} declares result {:?} but did not return it", function.name(), spec.name()),
                )
            })?;
            let value = value.coerce(spec.value_type()).map_err(|err| {
                fail(StatusCode::Internal, format!("{} result {:?}: {err}", function.name(), spec.name()))
            })?;
            out.results.insert(spec.name().to_owned(), value.to_proto());
        }
        if !results.is_empty() {
            let mut extra: Vec<_> = results.into_keys().collect();
            extra.sort();
            return Err(fail(
                StatusCode::Internal,
                format!("{} returned undeclared results {extra:?}", function.name()),
            ));
        }
        Ok(out)
    }

    // ---- health -----------------------------------------------------------

    fn device_health(&self, device: &Device) -> proto::DeviceHealth {
        use proto::device_health::State;
        let mut out = proto::DeviceHealth {
            device_id: device.id().to_owned(),
            state: State::Ok as i32,
            last_seen: self.last_seen.get(device.id()).copied().map(timestamp),
            ..Default::default()
        };
        if let Some(reported) = self.provider.device_health(device) {
            out.state = match reported.state {
                DeviceState::Ok => State::Ok,
                DeviceState::Stale => State::Stale,
                DeviceState::Unreachable => State::Unreachable,
                DeviceState::Fault => State::Fault,
            } as i32;
            out.message = reported.message;
            out.metrics = reported.metrics;
        }
        out
    }

    fn get_health(&self) -> proto::GetHealthResponse {
        use proto::provider_health::State;
        let health = self.provider.health();
        let (state, message) = if let HostCheck::Unmet(unmet) = &self.host_check {
            (State::Fault, format!("host requirements unmet: {unmet}"))
        } else if let Some(err) = &self.start_error {
            (State::Fault, format!("start failed: {err}"))
        } else {
            let state = match health.state {
                HealthState::Ok => State::Ok,
                HealthState::Degraded => State::Degraded,
                HealthState::Fault => State::Fault,
            };
            (state, health.message)
        };
        proto::GetHealthResponse {
            provider: Some(proto::ProviderHealth { state: state as i32, message, metrics: health.metrics }),
            devices: self.devices().iter().map(|d| self.device_health(d)).collect(),
        }
    }
}

fn kind(request: &Req) -> &'static str {
    match request {
        Req::Hello(_) => "hello",
        Req::ListDevices(_) => "list_devices",
        Req::DescribeDevice(_) => "describe_device",
        Req::ReadSignals(_) => "read_signals",
        Req::Call(_) => "call",
        Req::GetHealth(_) => "get_health",
        Req::WaitReady(_) => "wait_ready",
    }
}

/// semantics.md §6.2 and §8.3: a nonzero `function_id` wins, and `function_name`
/// is then ignored.
fn resolve_function<'a>(
    device: &'a Device,
    request: &proto::CallRequest,
) -> Result<&'a crate::Function, ProviderError> {
    let dtype = device.device_type();
    if request.function_id != 0 {
        return dtype.function_by_id(request.function_id).ok_or_else(|| {
            fail(
                StatusCode::NotFound,
                format!("unknown function_id {} on device {}", request.function_id, quote(device.id())),
            )
        });
    }
    if !request.function_name.is_empty() {
        return dtype.function_by_name(&request.function_name).ok_or_else(|| {
            fail(
                StatusCode::NotFound,
                format!("unknown function_name {} on device {}", quote(&request.function_name), quote(device.id())),
            )
        });
    }
    Err(fail(StatusCode::NotFound, "the call names no function (function_id 0 and no function_name)"))
}

/// semantics.md §8.3: no undeclared argument, every required one present, and
/// each value of its type, finite and within its bounds.
fn validate_args(function: &crate::Function, raw: &HashMap<String, proto::Value>) -> Result<Args, ProviderError> {
    let mut undeclared: Vec<&String> = raw.keys().filter(|k| function.args().iter().all(|a| a.name() != *k)).collect();
    undeclared.sort();
    if let Some(name) = undeclared.first() {
        return Err(fail(
            StatusCode::InvalidArgument,
            format!("{} takes no argument {}", function.name(), quote(name)),
        ));
    }
    let mut args = HashMap::new();
    for arg in function.args() {
        match raw.get(arg.name()) {
            None if arg.is_required() => {
                return Err(fail(
                    StatusCode::InvalidArgument,
                    format!("{} requires argument {:?}", function.name(), arg.name()),
                ));
            }
            None => {}
            Some(value) => {
                args.insert(arg.name().to_owned(), arg.validate(value)?);
            }
        }
    }
    Ok(Args(args))
}

/// The session and whether `stop` has run, shared with the SIGTERM handler.
pub(crate) struct Shared<P: Provider> {
    session: Mutex<Session<P>>,
    stopped: AtomicBool,
}

impl<P: Provider> Shared<P> {
    pub(crate) fn new(session: Session<P>) -> Self {
        Self { session: Mutex::new(session), stopped: AtomicBool::new(false) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session<P>> {
        // A panic in the provider is caught inside handle, so the lock is not
        // left poisoned by one; recover anyway rather than skip stop().
        self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Run the provider's `stop` the first time this is called, and never again.
    pub(crate) fn stop(&self) {
        let mut session = self.lock();
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| session.provider.stop())) {
            eprintln!("{}: ERROR provider stop panicked: {}", P::NAME, panic_message(payload.as_ref()));
        }
    }

    /// Serve requests until the stream ends. Returns the process exit code:
    /// 0 on a clean EOF or a closed output, 2 on a framing or I/O error, 3 on
    /// an unparseable request (framed-stdio v1 §4). Does not call `stop`.
    pub(crate) fn run_loop(&self, mut input: impl Read, mut output: impl Write) -> u8 {
        loop {
            let payload = match read_frame(&mut input) {
                Ok(Some(payload)) => payload,
                Ok(None) => return 0,
                Err(err) => {
                    eprintln!("{}: ERROR framing error: {err}", P::NAME);
                    return 2;
                }
            };
            let request = match proto::Request::decode(payload.as_slice()) {
                Ok(request) => request,
                Err(err) => {
                    eprintln!("{}: ERROR unparseable request: {err}", P::NAME);
                    return 3;
                }
            };
            let request_id = request.request_id;
            let mut data = self.lock().handle(request).encode_to_vec();
            if data.len() > MAX_FRAME_BYTES {
                let too_big = proto::Response {
                    request_id,
                    status: Some(proto::Status {
                        code: proto::status::Code::ResourceExhausted as i32,
                        message: format!(
                            "response of {} bytes exceeds the {MAX_FRAME_BYTES}-byte frame limit",
                            data.len()
                        ),
                        details: HashMap::new(),
                    }),
                    payload: None,
                };
                data = too_big.encode_to_vec();
            }
            match write_frame(&mut output, &data) {
                Ok(()) => {}
                Err(FrameError::Io(err)) if err.kind() == ErrorKind::BrokenPipe => return 0,
                Err(err) => {
                    eprintln!("{}: ERROR writing a response: {err}", P::NAME);
                    return 2;
                }
            }
        }
    }
}

/// Run a session over framed streams until the input ends, then call the
/// provider's `stop` exactly once. Returns the process exit code: 0 on a clean
/// EOF or a closed output, 2 on a framing error, 3 on an unparseable request
/// (framed-stdio v1 §4).
///
/// [`crate::run`] is the entry point for a provider executable; it also handles
/// SIGTERM and keeps stray output off the frame stream. This is the loop alone,
/// for tests and embedding.
pub fn serve<P: Provider>(provider: P, config: P::Config, input: impl Read, output: impl Write) -> u8 {
    let shared = Shared::new(Session::new(provider, config));
    let code = shared.run_loop(input, output);
    shared.stop();
    code
}
