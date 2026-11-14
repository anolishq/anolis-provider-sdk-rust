//! Protocol rules the conformance harness cannot observe from outside the
//! process: whether the provider was invoked, how its own mistakes are
//! reported, and the session's lifecycle.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use anolis_provider_sdk::proto::{self, request::Payload as Req, response::Payload as Resp, status::Code};
use anolis_provider_sdk::{
    Arg, Args, BoxError, CallContext, Device, DeviceType, Function, HostRequirement, Provider, ProviderError, Readings,
    Results, Session, Signal, StatusCode, Value, ValueType, write_frame,
};
use prost::Message;
use schemars::JsonSchema;
use serde::Deserialize;

static TOY: LazyLock<Arc<DeviceType>> = LazyLock::new(|| {
    DeviceType::new("toy")
        .signal(Signal::new("level", ValueType::Uint64))
        .signal(Signal::new("boom", ValueType::Bool).not_default())
        .function(Function::new("bump").result(Arg::int64("calls")))
        .function(Function::new("slow").arg(Arg::uint64("ms").range(0, 5000)))
        .function(Function::new("slow_fail").arg(Arg::uint64("ms").range(0, 5000)))
        .function(Function::new("no_result").result(Arg::int64("n")))
        .function(Function::new("extra_result"))
        .build()
});

#[derive(Clone, Default, Deserialize, JsonSchema)]
struct Config {
    #[serde(default)]
    unmet_host: bool,
}

#[derive(Default)]
struct Toy {
    devices: Vec<Device>,
    calls: i64,
    level: i64,
    stops: Arc<AtomicUsize>,
}

impl Provider for Toy {
    type Config = Config;
    const NAME: &'static str = "toy";
    const VERSION: &'static str = "0.0.1";

    fn new(_config: Config) -> Result<Self, BoxError> {
        Ok(Self { devices: vec![Device::new("t0", &TOY)], ..Default::default() })
    }

    fn check_host(config: &Config) -> Result<Vec<HostRequirement>, BoxError> {
        Ok(if config.unmet_host {
            vec![HostRequirement::unmet("bus.access").detail("/dev/i2c-1 is missing")]
        } else {
            vec![HostRequirement::met("bus.access")]
        })
    }

    fn devices(&self) -> &[Device] {
        &self.devices
    }

    fn read(&mut self, _device: &Device, signal_ids: &[&str]) -> Result<Readings, ProviderError> {
        if signal_ids.contains(&"boom") {
            panic!("driver bug");
        }
        // An INT64 value for a UINT64 signal: the SDK converts it when it fits.
        Ok(signal_ids.iter().map(|&id| (id.to_owned(), self.level.into())).collect())
    }

    fn call(
        &mut self,
        _device: &Device,
        function: &Function,
        args: &Args,
        _ctx: &CallContext,
    ) -> Result<Results, ProviderError> {
        self.calls += 1;
        match function.name() {
            "bump" => Ok(Results::from([("calls".to_owned(), Value::from(self.calls))])),
            "slow" | "slow_fail" => {
                sleep(Duration::from_millis(args.u64("ms").unwrap()));
                if function.name() == "slow_fail" {
                    return Err(ProviderError::unavailable("bus timed out"));
                }
                Ok(Results::new())
            }
            "extra_result" => Ok(Results::from([("surprise".to_owned(), Value::from(1))])),
            _ => Ok(Results::new()),
        }
    }

    fn stop(&mut self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
}

fn session_with(config: Config) -> Session<Toy> {
    let mut s = Session::new(Toy::new(config.clone()).unwrap(), config);
    let hello = proto::HelloRequest { protocol_version: "v1".into(), ..Default::default() };
    assert_eq!(code(&s.handle(request(Req::Hello(hello)))), Code::Ok);
    s
}

fn session() -> Session<Toy> {
    session_with(Config::default())
}

fn request(payload: Req) -> proto::Request {
    proto::Request { request_id: 7, payload: Some(payload) }
}

fn code(response: &proto::Response) -> Code {
    Code::try_from(response.status.as_ref().unwrap().code).unwrap()
}

fn call(
    s: &mut Session<Toy>,
    function: &str,
    args: &[(&str, proto::Value)],
    deadline: Option<SystemTime>,
) -> proto::Response {
    s.handle(request(Req::Call(proto::CallRequest {
        device_id: "t0".into(),
        function_name: function.into(),
        args: args.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect(),
        deadline: deadline.map(|d| {
            let since = d.duration_since(SystemTime::UNIX_EPOCH).unwrap();
            prost_types::Timestamp { seconds: since.as_secs() as i64, nanos: since.subsec_nanos() as i32 }
        }),
        ..Default::default()
    })))
}

fn uint(u: u64) -> proto::Value {
    proto::Value { r#type: proto::ValueType::Uint64 as i32, kind: Some(proto::value::Kind::Uint64Value(u)) }
}

#[test]
fn an_expired_deadline_never_invokes_the_function() {
    let mut s = session();
    let past = SystemTime::now() - Duration::from_secs(1);
    let response = call(&mut s, "bump", &[], Some(past));
    assert_eq!(code(&response), Code::DeadlineExceeded);
    assert_eq!(s.provider().calls, 0, "§8.4: no side effects for an expired deadline");
}

#[test]
fn a_deadline_passed_during_the_call_is_deadline_exceeded_with_possible_side_effects() {
    let mut s = session();
    let deadline = SystemTime::now() + Duration::from_millis(50);
    let response = call(&mut s, "slow", &[("ms", uint(150))], Some(deadline));
    assert_eq!(code(&response), Code::DeadlineExceeded);
    assert_eq!(response.status.unwrap().details["side_effects"], "possible");
    assert!(response.payload.is_none());
}

#[test]
fn a_deadline_passed_during_a_failed_call_is_still_deadline_exceeded() {
    let mut s = session();
    let deadline = SystemTime::now() + Duration::from_millis(50);
    let response = call(&mut s, "slow_fail", &[("ms", uint(150))], Some(deadline));
    assert_eq!(code(&response), Code::DeadlineExceeded);
    let details = response.status.unwrap().details;
    assert_eq!(details["side_effects"], "possible");
    assert_eq!(details["call_error"], "bus timed out");
}

#[test]
fn a_call_within_its_deadline_succeeds() {
    let mut s = session();
    let deadline = SystemTime::now() + Duration::from_secs(5);
    assert_eq!(code(&call(&mut s, "slow", &[("ms", uint(1))], Some(deadline))), Code::Ok);
}

#[test]
fn a_missing_declared_result_is_internal() {
    let mut s = session();
    let response = call(&mut s, "no_result", &[], None);
    assert_eq!(code(&response), Code::Internal);
    assert!(response.status.unwrap().message.contains("declares result \"n\""));
}

#[test]
fn an_undeclared_result_is_internal() {
    let mut s = session();
    let response = call(&mut s, "extra_result", &[], None);
    assert_eq!(code(&response), Code::Internal);
    assert!(response.status.unwrap().message.contains("undeclared results [\"surprise\"]"));
}

#[test]
fn a_panic_in_the_provider_is_internal_and_the_session_goes_on() {
    let mut s = session();
    let read = |ids: &[&str]| {
        Req::ReadSignals(proto::ReadSignalsRequest {
            device_id: "t0".into(),
            signal_ids: ids.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        })
    };
    let response = s.handle(request(read(&["boom"])));
    assert_eq!(code(&response), Code::Internal);
    assert!(response.status.unwrap().message.contains("driver bug"));
    assert_eq!(code(&s.handle(request(read(&[])))), Code::Ok);
}

#[test]
fn a_value_that_does_not_fit_its_signal_is_internal() {
    let mut s = session();
    s.provider_mut().level = -1;
    let response =
        s.handle(request(Req::ReadSignals(proto::ReadSignalsRequest { device_id: "t0".into(), ..Default::default() })));
    assert_eq!(code(&response), Code::Internal);
    assert!(response.status.unwrap().message.contains("does not fit UINT64"));
}

#[test]
fn an_unmet_host_requirement_keeps_the_provider_up_but_not_ready() {
    let mut s = session_with(Config { unmet_host: true });
    let ready = s.handle(request(Req::WaitReady(proto::WaitReadyRequest::default())));
    let Some(Resp::WaitReady(ready)) = ready.payload else { panic!("no WaitReady payload") };
    assert_eq!(ready.diagnostics["ready"], "false");
    assert_eq!(ready.diagnostics["host_check"], "unmet");
    assert_eq!(ready.diagnostics["host_unmet"], "bus.access: /dev/i2c-1 is missing");
    assert_eq!(ready.diagnostics["device_count"], "0");
    let health = s.handle(request(Req::GetHealth(proto::GetHealthRequest {})));
    let Some(Resp::GetHealth(health)) = health.payload else { panic!("no GetHealth payload") };
    let provider = health.provider.unwrap();
    assert_eq!(provider.state, proto::provider_health::State::Fault as i32);
    assert!(provider.message.contains("host requirements unmet"));
}

#[test]
fn a_met_host_check_is_reported_ok() {
    let mut s = session();
    let ready = s.handle(request(Req::WaitReady(proto::WaitReadyRequest::default())));
    let Some(Resp::WaitReady(ready)) = ready.payload else { panic!("no WaitReady payload") };
    assert_eq!(ready.diagnostics["host_check"], "ok");
    assert_eq!(ready.diagnostics["ready"], "true");
    assert!(ready.diagnostics.contains_key("init_time_ms"));
}

#[test]
fn serve_stops_the_provider_exactly_once_and_exits_0_on_eof() {
    let stops = Arc::new(AtomicUsize::new(0));
    let provider = Toy { stops: Arc::clone(&stops), ..Toy::new(Config::default()).unwrap() };
    let mut input = Vec::new();
    let hello = request(Req::Hello(proto::HelloRequest { protocol_version: "v1".into(), ..Default::default() }));
    write_frame(&mut input, &hello.encode_to_vec()).unwrap();
    let mut output = Vec::new();
    assert_eq!(anolis_provider_sdk::serve(provider, Config::default(), input.as_slice(), &mut output), 0);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(!output.is_empty());
}

#[test]
fn serve_stops_the_provider_on_a_framing_error_too() {
    let stops = Arc::new(AtomicUsize::new(0));
    let provider = Toy { stops: Arc::clone(&stops), ..Toy::new(Config::default()).unwrap() };
    let code = anolis_provider_sdk::serve(provider, Config::default(), &[1u8, 0][..], Vec::new());
    assert_eq!(code, 2);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
}

#[test]
fn provider_errors_keep_their_code_and_details() {
    let err = ProviderError::new(StatusCode::Unavailable, "gone").detail("bus", "i2c-1");
    assert_eq!(err.details["bus"], "i2c-1");
}
