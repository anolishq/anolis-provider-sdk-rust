# anolis-provider-sdk-rust dev commands. `check` and `test` cover CI's check
# and test jobs; CI also runs proto-check, version-sync and the org docs check
# (markdownlint, a VitePress build, an offline link check), which has no recipe.

# The anolis-protocol release the vendored proto/ tree and the harness come from.
protocol := "1.10.0"
protocol_sha256 := "77c8baf8b423682e2f1fda771540a24ff0cb0843a3d7540607991f662218419a"
release := "https://github.com/anolishq/anolis-protocol/releases/download/v" + protocol
harness_wheel := release + "/anolis_protocol-" + protocol + "-py3-none-any.whl"
reference := "target/debug/examples/anolis-provider-reference"

# List available recipes
default:
    @just --list

# Install the toolchain components and the conformance harness
setup:
    rustup component add rustfmt clippy
    test -x .harness/bin/anolis-adpp-conformance || (python3 -m venv .harness && .harness/bin/pip install -q "anolis-protocol[conformance] @ {{harness_wheel}}")

# Format the code
fmt:
    cargo fmt --all

# Check formatting without changing files
fmt-check:
    cargo fmt --all --check

# Lint, warnings as errors
lint:
    cargo clippy --locked --all-targets --all-features -- -D warnings

# CI-equivalent static checks: format, lint, generated bindings in sync
check: fmt-check lint gen-check

# Unit and integration tests, then the conformance harness against the reference provider
test: && conformance
    cargo test --locked

# The anolis-protocol conformance harness against the reference provider: no failures, no unlisted skips
conformance: setup
    cargo build --locked --example anolis-provider-reference
    python3 scripts/conformance.py {{reference}} .harness/bin/anolis-adpp-conformance

# Regenerate src/proto/ from the vendored proto/ tree
gen:
    cargo run --locked --quiet -p protogen

# Fail when src/proto/ is not what `just gen` produces
gen-check: gen
    @test -z "$(git status --porcelain -- src/proto)" || (git status --short -- src/proto; echo "src/proto/ is out of date: run just gen and commit it"; exit 1)

# Fail when proto/ differs from the pinned anolis-protocol release
proto-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    trap 'rm -rf -- "$tmp"' EXIT
    curl -fsSL -o "$tmp/src.tar.gz" "{{release}}/anolis-protocol-{{protocol}}-source.tar.gz"
    echo "{{protocol_sha256}}  $tmp/src.tar.gz" | sha256sum -c --quiet
    tar -xzf "$tmp/src.tar.gz" -C "$tmp"
    diff -r "$tmp/anolis-protocol-{{protocol}}/proto" proto
    echo "proto/ matches anolis-protocol v{{protocol}}"
