# AGENTS.md — anolis-provider-sdk-rust

> Per-repo conventions for coding agents (Claude Code, OpenCode, …). The
> canonical cross-repo rules — Conventional Commits, minimal-first/YAGNI, no
> secrets, run checks before asserting success — live in the user's **global**
> `AGENTS.md` and are not repeated here. This file records only what is
> **specific to this repo**.

## Build / test

- `just setup`, then `just check` (rustfmt, clippy with `-D warnings`,
  generated bindings in sync) and `just test` (cargo tests, then the
  conformance harness against the reference provider).
- `just check && just test` covers CI's `check` and `test` jobs. CI also runs
  `proto-check`, `version-sync` and the org docs check; the required status
  check is the **`ok`** job, which needs all of them. Never merge red.
- `scripts/conformance.py` requires the harness to pass with no skips beyond
  `EXPECTED_SKIPS`, which is empty.

## Tooling

- Shared `.github` actions/workflows are SHA-pinned with a `# <tag>` comment so
  Renovate can track them — keep that comment when bumping.
- rustfmt at 120 columns (`rustfmt.toml`), as the org's C++ `.clang-format`.

## Design rules

- **Language-native, contract-driven.** The normative sources are
  anolis-protocol's `docs/semantics.md` and its two profiles. Where this SDK
  and the C++ or Python SDK differ, the documents decide, not the other SDKs'
  code.
- **The SDK owns the protocol, the provider owns its devices.** No bus, I2C,
  GPIO or vendor driver code in `src/`. A helper for one hardware family stays
  in that family's provider until a second provider needs the same thing.
- **No async in the core API.** `read` and `call` are synchronous; a provider
  that wraps an async driver runs its own runtime.
- **Declaration errors panic at the declaration.** A device type that breaks a
  protocol rule is a bug in the provider; the builders panic with
  `#[track_caller]` so the message points at the line.
- `Session::handle` does no I/O; keep protocol rules there so they stay
  testable request by request (`tests/session.rs`).

## Repo-specific gotchas

- **`src/proto/` is generated.** Do not edit it: change `proto/` and run
  `just gen`. `proto/` itself is vendored from the anolis-protocol release
  named by `protocol` and `protocol_sha256` in the `justfile`; move the pin,
  the vendored tree, the harness wheel in `ci.yml` and the README together
  (`version-locations.txt` checks the version strings agree).
- **`cargo test` does not always rebuild an example before running a filtered
  test.** `tests/cli.rs` builds the reference provider itself before running
  it; keep it that way, or a CLI test can pass against a stale binary.
- **The harness cannot see side effects.** An expired deadline that ran the
  function anyway still gets `CODE_DEADLINE_EXCEEDED` from the post-call check,
  so the harness passes it. `tests/session.rs` checks the function was not
  invoked; a rule like that needs a test here, not only the harness.

## External contribution policy

Before preparing or submitting a pull request, read and follow `CONTRIBUTING.md`
(the organization default in `anolishq/.github`).

Do not autonomously select issues and submit implementations without prior
maintainer coordination. Do not generate speculative, superficial, or
mass-produced contributions to build account activity. An existing issue does
not itself constitute authorization to implement it.

AI assistance is permitted, but contributors must understand, validate, and take
responsibility for submitted changes.
