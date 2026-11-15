# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-11-15

### Added

- The SDK: declare device types (signals, functions, arguments, results) as
  data and implement `devices`, `read` and `call` on a `Provider`. The SDK
  handles framed stdio, the Hello gate, request dispatch, the default signal
  set, timestamps and quality, argument validation, declared results,
  deadlines, `WaitReady`, `GetHealth` and the executable-profile command line.
- Call deadlines as semantics §8.4 states them. An expired deadline is
  refused before the function runs. A call is given its deadline in
  `CallContext`, so it can stop early, and when the deadline passes during the
  call the response is `CODE_DEADLINE_EXCEEDED` with `side_effects = possible`,
  even when the call itself returned an error, which is kept in the
  `call_error` detail. The C++ and Python SDKs send the call's own error
  unchanged in that case (anolishq/anolis-provider-sdk#82).
- The config is typed: `Provider::Config` is deserialized with serde, and its
  JSON Schema, derived with schemars, is what `--config-schema` prints and what
  every config is validated against first. A duplicated YAML key, a NaN or
  infinity, a non-string key and a config that is not a mapping are refused.
- A reference provider (`anolis-provider-reference`, the Python SDK's
  synthetic counter). It passes the anolis-protocol 1.10.0 conformance harness
  at level 2 with no skipped tests (`just test`), and CI runs the harness on
  x86_64 and arm64.
- `proto/`, vendored from the anolis-protocol v1.10.0 release and checked
  against it in CI, with the bindings generated from it committed under
  `src/proto/`, so building needs no `protoc`.
