//! The reference provider's command line, run as a process (executable
//! profile §1, §2, §6), and the config rules the harness leaves to the
//! provider: what `--check-config` refuses beyond a document that does not parse.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// The reference provider, built from the current sources. `cargo test` does
/// not always rebuild an example before running a filtered test, so build it
/// here rather than test a stale binary.
fn reference() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "--quiet", "--example", "anolis-provider-reference", "--manifest-path"])
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .status()
            .unwrap();
        assert!(status.success(), "building the reference provider failed");
        let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        deps.parent().unwrap().join("examples").join("anolis-provider-reference")
    })
    .clone()
}

fn run(args: &[&str]) -> Output {
    Command::new(reference()).args(args).output().unwrap()
}

fn config_file(name: &str, text: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn check_config(name: &str, text: &str) -> (i32, String) {
    let path = config_file(name, text);
    let out = run(&["--check-config", path.to_str().unwrap()]);
    assert!(out.stdout.is_empty(), "--check-config wrote to stdout");
    (out.status.code().unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn version_prints_a_dotted_version() {
    let out = run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("anolis-provider-reference 0."), "{text}");
}

#[test]
fn config_schema_prints_the_envelope() {
    let out = run(&["--config-schema"]);
    assert_eq!(out.status.code(), Some(0));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["config_schema_version"], 1);
    assert_eq!(envelope["provider"], "anolis-provider-reference");
    assert_eq!(envelope["schema"]["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(envelope["schema"]["additionalProperties"], false);
}

#[test]
fn usage_errors_exit_2_with_nothing_on_stdout() {
    for args in [&[][..], &["--bogus"], &["--config"], &["--version", "extra"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn the_conformance_config_is_valid() {
    let out = run(&["--check-config", concat!(env!("CARGO_MANIFEST_DIR"), "/config/conformance.yaml")]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn an_empty_config_is_an_empty_mapping() {
    assert_eq!(check_config("empty.yaml", "").0, 0);
}

#[test]
fn a_json_config_is_read_too() {
    assert_eq!(check_config("config.json", r#"{"devices": [{"id": "c0"}]}"#).0, 0);
}

#[test]
fn a_missing_file_is_invalid() {
    let out = run(&["--check-config", "/nonexistent/config.yaml"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn a_duplicated_key_is_refused() {
    let (code, err) = check_config("dup.yaml", "devices:\n  - id: a\n    id: b\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("duplicate"), "{err}");
}

#[test]
fn a_non_finite_number_is_refused() {
    let (code, err) = check_config("nan.yaml", "devices:\n  - id: a\n    start: .nan\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("devices/0/start: a number must be finite"), "{err}");
}

#[test]
fn a_float_where_an_integer_is_declared_is_refused() {
    let (code, err) = check_config("float.yaml", "devices:\n  - id: a\n    start: 2.0\n");
    assert_eq!(code, 1, "{err}");
}

#[test]
fn an_unknown_key_is_refused() {
    let (code, err) = check_config("unknown.yaml", "devices:\n  - id: a\n    labl: typo\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("does not match the config schema"), "{err}");
}

#[test]
fn a_schema_violation_names_where() {
    let (code, err) = check_config("pattern.yaml", "devices:\n  - id: Not-Snake\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("/devices/0/id"), "{err}");
}

#[test]
fn a_config_that_is_not_a_mapping_is_refused() {
    let (code, err) = check_config("list.yaml", "- a\n- b\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("must be a mapping, not a list"), "{err}");
}

#[test]
fn check_config_runs_the_providers_own_checks() {
    let (code, err) = check_config("twice.yaml", "devices:\n  - id: a\n  - id: a\n");
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("configured twice"), "{err}");
}

#[test]
fn check_host_prints_the_envelope() {
    let path = config_file("host.yaml", "devices: []\n");
    let out = run(&["--check-host", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["check_host_version"], 1);
    assert_eq!(envelope["requirements"], serde_json::json!([]));
}

#[test]
fn check_host_on_an_invalid_config_exits_2_with_nothing_on_stdout() {
    let path = config_file("host-bad.yaml", "devices: 3\n");
    let out = run(&["--check-host", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn sigterm_ends_the_session_with_exit_0() {
    use anolis_provider_sdk::proto::{self, request::Payload};
    use prost::Message;
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(reference())
        .args(["--config", concat!(env!("CARGO_MANIFEST_DIR"), "/config/conformance.yaml")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // An answered Hello means the handler is installed and the loop is serving.
    let hello = proto::Request {
        request_id: 1,
        payload: Some(Payload::Hello(proto::HelloRequest { protocol_version: "v1".into(), ..Default::default() })),
    };
    let mut stdin = child.stdin.take().unwrap();
    anolis_provider_sdk::write_frame(&mut stdin, &hello.encode_to_vec()).unwrap();
    let frame = anolis_provider_sdk::read_frame(child.stdout.as_mut().unwrap()).unwrap().unwrap();
    assert_eq!(proto::Response::decode(frame.as_slice()).unwrap().request_id, 1);
    // SAFETY: signalling a child this test spawned and still owns.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let status = child.wait().unwrap();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert_eq!(status.code(), Some(0), "{err}");
    assert!(err.contains("SIGTERM: ending the session"), "{err}");
}
