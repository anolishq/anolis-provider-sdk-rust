//! The provider executable's command line (anolis-protocol
//! `docs/profiles/anolis-executable-profile-v1.md` §1, §2, §6).
//!
//! Exit codes: 0 success; 1 an invalid config (`--check-config`, `--config`)
//! or an unmet host requirement (`--check-host`); 2 a usage error, or
//! `--check-host` that could not evaluate (an invalid config, or `check_host`
//! failed). Only the JSON envelopes and `--version` ever go to stdout.

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Map, Value as Json, json};

use crate::model::HostStatus;
use crate::provider::Provider;
use crate::session::{Session, Shared};

const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

fn usage(name: &str) -> String {
    format!(
        "usage:\n  \
         {name} --config <path>         serve ADPP over stdin/stdout\n  \
         {name} --check-config <path>   validate a config; exit 0 valid, 1 invalid\n  \
         {name} --check-host <path>     report the host requirements a config implies\n  \
         {name} --config-schema         print the config JSON Schema envelope\n  \
         {name} --version               print the version\n"
    )
}

/// Why a config was refused.
#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// The provider's config schema, with the dialect declared when it was not.
pub fn config_schema<P: Provider>() -> Json {
    let mut schema = P::config_schema();
    if let Json::Object(map) = &mut schema {
        map.entry("$schema").or_insert_with(|| Json::String(SCHEMA_DIALECT.to_owned()));
    }
    schema
}

/// YAML as JSON. YAML parsers disagree on which occurrence of a duplicated key
/// wins, so a duplicate is refused, as the C++ and Python SDKs do; JSON has no
/// NaN or infinity, so neither is accepted; and a key must be a string.
fn yaml_to_json(value: serde_norway::Value, path: &mut Vec<String>) -> Result<Json, String> {
    use serde_norway::Value as Y;
    let at = |path: &[String]| if path.is_empty() { "(root)".to_owned() } else { path.join("/") };
    Ok(match value {
        Y::Null => Json::Null,
        Y::Bool(b) => Json::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Json::from(i)
            } else if let Some(u) = n.as_u64() {
                Json::from(u)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                serde_json::Number::from_f64(f)
                    .map(Json::Number)
                    .ok_or_else(|| format!("{}: a number must be finite (JSON has no NaN or infinity)", at(path)))?
            }
        }
        Y::String(s) => Json::String(s),
        Y::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.into_iter().enumerate() {
                path.push(i.to_string());
                out.push(yaml_to_json(item, path)?);
                path.pop();
            }
            Json::Array(out)
        }
        Y::Mapping(mapping) => {
            let mut out = Map::new();
            for (key, item) in mapping {
                let Y::String(key) = key else {
                    return Err(format!("{}: a key must be a string", at(path)));
                };
                path.push(key.clone());
                let converted = yaml_to_json(item, path)?;
                path.pop();
                out.insert(key, converted);
            }
            Json::Object(out)
        }
        Y::Tagged(tagged) => yaml_to_json(tagged.value, path)?,
    })
}

/// Read a YAML (or JSON) config, validate it against the provider's schema
/// and `check_config`, and deserialize it.
pub fn load_config<P: Provider>(path: &Path) -> Result<P::Config, ConfigError> {
    let shown = path.display();
    let text = std::fs::read(path).map_err(|err| ConfigError(format!("cannot read {shown}: {err}")))?;
    let text = String::from_utf8(text).map_err(|err| {
        ConfigError(format!("{shown} is not UTF-8 text: invalid byte at {}", err.utf8_error().valid_up_to()))
    })?;
    let yaml: serde_norway::Value =
        serde_norway::from_str(&text).map_err(|err| ConfigError(format!("{shown} is not valid YAML: {err}")))?;
    let data = match yaml_to_json(yaml, &mut Vec::new()).map_err(|err| ConfigError(format!("{shown}: {err}")))? {
        Json::Null => Json::Object(Map::new()),
        Json::Object(map) => Json::Object(map),
        other => {
            let kind = match other {
                Json::Array(_) => "list",
                Json::String(_) => "string",
                Json::Number(_) => "number",
                _ => "boolean",
            };
            return Err(ConfigError(format!("{shown}: the config must be a mapping, not a {kind}")));
        }
    };
    let schema = config_schema::<P>();
    let validator = jsonschema::validator_for(&schema)
        .map_err(|err| ConfigError(format!("{} has an invalid config schema: {err}", P::NAME)))?;
    let mut errors: Vec<(String, String)> = validator
        .iter_errors(&data)
        .map(|e| {
            let at = e.instance_path().to_string();
            (if at.is_empty() { "(root)".to_owned() } else { at }, e.to_string())
        })
        .collect();
    if !errors.is_empty() {
        errors.sort();
        let lines: Vec<String> = errors.iter().map(|(at, msg)| format!("  {at}: {msg}")).collect();
        return Err(ConfigError(format!("{shown} does not match the config schema:\n{}", lines.join("\n"))));
    }
    let config: P::Config = serde_json::from_value(data).map_err(|err| ConfigError(format!("{shown}: {err}")))?;
    P::check_config(&config).map_err(|err| ConfigError(format!("{shown}: {err}")))?;
    Ok(config)
}

/// Entry point for a provider executable:
///
/// ```no_run
/// # use anolis_provider_sdk::*;
/// # fn demo<MyProvider: Provider>() -> std::process::ExitCode {
/// // fn main() -> std::process::ExitCode {
///     anolis_provider_sdk::run::<MyProvider>()
/// // }
/// # }
/// ```
pub fn run<P: Provider>() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(run_args::<P>(&args))
}

fn run_args<P: Provider>(args: &[String]) -> u8 {
    let name = P::NAME;
    match args {
        [flag] if flag == "--version" => {
            println!("{name} {}", P::VERSION);
            return 0;
        }
        [flag] if flag == "--config-schema" => {
            let envelope = json!({"config_schema_version": 1, "provider": name, "schema": config_schema::<P>()});
            println!("{}", serde_json::to_string_pretty(&envelope).expect("a JSON value serializes"));
            return 0;
        }
        [verb, _] if ["--config", "--check-config", "--check-host"].contains(&verb.as_str()) => {}
        _ => {
            eprint!("{}", usage(name));
            return 2;
        }
    }
    let (verb, path) = (args[0].as_str(), Path::new(&args[1]));
    let config = match load_config::<P>(path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{name}: {err}");
            // --check-host could not evaluate (§6: exit 2); otherwise the config is invalid.
            return if verb == "--check-host" { 2 } else { 1 };
        }
    };
    match verb {
        "--check-config" => {
            eprintln!("{name}: config valid");
            0
        }
        "--check-host" => check_host::<P>(&config),
        _ => serve_stdio::<P>(config),
    }
}

fn check_host<P: Provider>(config: &P::Config) -> u8 {
    let requirements = match P::check_host(config) {
        Ok(requirements) => requirements,
        Err(err) => {
            eprintln!("{}: could not evaluate the host requirements: {err}", P::NAME);
            return 2; // §6: could not evaluate
        }
    };
    let entries: Vec<Json> = requirements
        .iter()
        .map(|r| {
            let mut entry = Map::new();
            entry.insert("id".to_owned(), json!(r.id));
            entry.insert("status".to_owned(), json!(r.status.as_str()));
            if !r.detail.is_empty() {
                entry.insert("detail".to_owned(), json!(r.detail));
            }
            if !r.remedy.is_empty() {
                entry.insert("remedy".to_owned(), json!(r.remedy));
            }
            Json::Object(entry)
        })
        .collect();
    let envelope = json!({"check_host_version": 1, "provider": P::NAME, "requirements": entries});
    println!("{}", serde_json::to_string_pretty(&envelope).expect("a JSON value serializes"));
    u8::from(requirements.iter().any(|r| r.status == HostStatus::Unmet))
}

fn serve_stdio<P: Provider>(config: P::Config) -> u8 {
    let provider = match P::new(config.clone()) {
        Ok(provider) => provider,
        Err(err) => {
            eprintln!("{}: ERROR provider failed to initialise: {err}", P::NAME);
            return 1;
        }
    };
    let shared = std::sync::Arc::new(Shared::new(Session::new(provider, config)));
    let frames_out = stdio::take_stdout();
    stdio::on_sigterm(std::sync::Arc::clone(&shared));
    let code = shared.run_loop(std::io::stdin().lock(), frames_out);
    shared.stop();
    code
}

#[cfg(unix)]
mod stdio {
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::sync::Arc;

    use crate::provider::Provider;
    use crate::session::Shared;

    /// stdout carries frames only. Frames go to a private duplicate of fd 1,
    /// and fd 1 itself is pointed at stderr, so a stray `println!`, a C
    /// library's `printf` or a child process that inherits stdout lands in the
    /// log instead of corrupting the frame stream.
    pub(super) fn take_stdout() -> File {
        std::io::stdout().flush().ok();
        // SAFETY: dup and dup2 on the process's own standard descriptors. The
        // duplicate is a fresh descriptor that nothing else owns, so the File
        // takes sole ownership of it.
        unsafe {
            let frames = libc::dup(1);
            assert!(frames >= 0, "dup(1) failed: {}", std::io::Error::last_os_error());
            assert!(libc::dup2(2, 1) >= 0, "dup2(2, 1) failed: {}", std::io::Error::last_os_error());
            File::from_raw_fd(frames)
        }
    }

    /// On SIGTERM, end the session in order: wait for a request in progress,
    /// run the provider's `stop` once, and exit 0.
    pub(super) fn on_sigterm<P: Provider>(shared: Arc<Shared<P>>) {
        let mut signals = match signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM]) {
            Ok(signals) => signals,
            Err(err) => {
                eprintln!("{}: WARNING cannot handle SIGTERM: {err}", P::NAME);
                return;
            }
        };
        std::thread::spawn(move || {
            if signals.forever().next().is_some() {
                eprintln!("{}: INFO SIGTERM: ending the session", P::NAME);
                shared.stop();
                std::process::exit(0);
            }
        });
    }
}

#[cfg(not(unix))]
mod stdio {
    use std::sync::Arc;

    use crate::provider::Provider;
    use crate::session::Shared;

    pub(super) fn take_stdout() -> std::io::Stdout {
        std::io::stdout()
    }

    pub(super) fn on_sigterm<P: Provider>(_shared: Arc<Shared<P>>) {}
}
