# anolis-provider-sdk-rust

A Rust SDK for writing [Anolis Device Provider Protocol](https://github.com/anolishq/anolis-protocol)
(ADPP) device providers.

> **Status: v0.1.0, unreleased.** The SDK core and a synthetic reference
> provider pass the anolis-protocol v1.10.0 conformance harness at level 2
> with no failures and no skips (`just test`); CI runs it on x86_64 and arm64.

Declare each device type once as data, implement `Provider`, and end the
binary with `run`. The SDK speaks ADPP v1 over framed stdio and implements the
Anolis executable profile's command line. The provider owns its devices and
its driver.

```rust
fn main() -> std::process::ExitCode {
    anolis_provider_sdk::run::<MyProvider>()
}
```

## What it provides

- **The protocol**: framed stdio (`uint32_le` length prefix, 1 MiB cap), the
  Hello gate, request dispatch, the §7.2 default signal set, timestamps and
  quality (STALE past `stale_after_ms`), §8.3 argument validation, declared
  call results, §8.4 deadlines, `WaitReady` and `GetHealth`.
- **The executable profile**: `--config`, `--check-config`, `--config-schema`,
  `--check-host` and `--version`, with a config schema derived from the
  provider's config type, and the host checks run again at startup.
- **Process hygiene**: `stop` runs exactly once however the session ends,
  SIGTERM ends it in order with exit 0, and stray writes to stdout go to the
  log instead of the frame stream.

It has no bus, I2C, GPIO or vendor driver code, and no async runtime: `read`
and `call` are synchronous.

## Use it

The SDK is not on crates.io. Depend on it by git tag:

```toml
[dependencies]
anolis-provider-sdk = { git = "https://github.com/anolishq/anolis-provider-sdk-rust", tag = "v0.1.0" }
schemars = "1"
serde = { version = "1", features = ["derive"] }
```

[Writing a provider](docs/writing-a-provider.md) walks through a provider, and
[`examples/reference.rs`](examples/reference.rs) is a complete one.

## Develop

```bash
just setup   # rustfmt, clippy, and the conformance harness in .harness/
just check   # format, lint, generated bindings in sync
just test    # unit and integration tests, then the conformance harness
```

The protocol bindings in `src/proto/` are generated from `proto/`, which is
vendored from the anolis-protocol v1.10.0 release; `just gen` regenerates them
and `just proto-check` compares `proto/` with that release.

## License

Apache-2.0. See [LICENSE](LICENSE).
