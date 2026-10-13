# Contributing to loboot

loboot changes real native service definitions and runtime state. Contributions should keep behavior explicit, ownership conservative, and failure evidence intact. The project is early v0.1 and dual-licensed under MIT OR Apache-2.0.

## Before changing behavior

Read the [specification](docs/SPEC.md), [architecture](docs/ARCHITECTURE.md), and [CLI contract](docs/CLI.md). Explain material behavior/scope changes before implementing them and update the affected design/operator documents with the change. Small documentation corrections and behavior-preserving fixes do not need a separate planning cycle.

The documentation structure and separation of verification from release readiness are informed by [locron](https://github.com/WhiteKiwi/locron). Its daemon lifecycle, credentials, and platform claims do not automatically apply here.

## Build and check

Use the Rust 1.99.0 toolchain pinned in `rust-toolchain.toml` on macOS or Linux:

```sh
cargo build --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 scripts/check_docs.py
```

Use `--offline` for Cargo when dependencies are already cached. CI also runs stable Rust tests on Linux/macOS and a pinned 1.99.0 Linux compatibility lane. [Release process](docs/RELEASE.md) describes the separate artifact rehearsal and publication gates.

Ordinary tests and validation CI must never register, enable, start, stop, or remove a real host job. The release-readiness workflow explicitly opts into `scripts/native_smoke.py` only on disposable GitHub-hosted runners, recording passed, skipped, or failed lifecycle evidence separately. Use fake `Runner` implementations for native command sequences and `--root` sandboxes for filesystem/CLI checks. Native acceptance needs explicit authorization and an isolated disposable login account; follow [the protocol](docs/IMPLEMENTATION.md#opt-in-isolated-native-validation-protocol-pending).

## Where changes belong

- `src/model.rs`: strict manifest validation.
- `src/backend.rs`: rendering, native argv plans, state parsing, and runner boundary.
- `src/store.rs`: owned records, exact bytes, locking, guarded files, and journals.
- `src/main.rs`: CLI dispatch, live preconditions, offline mode, and output contract.
- `tests/`: subprocess coverage without live manager calls.
- `docs/`: user workflows, technical contracts, and verification evidence.

Add regression coverage for refusal paths as well as success: malformed inputs, symlinks, drift/foreign files, stale plans, interrupted writes, native partial failures, disabled/loaded macOS combinations, dry-run side effects, and JSON purity. Keep unsafe filesystem operations small and justify their invariants; the current implementation uses Unix descriptor APIs deliberately.

## A reviewable pull request

State what changed, why, and what remains out of scope. Include exact commands/results and distinguish local tests, hosted CI, offline smoke, and real native validation. Do not call an unrun check a pass. Update the [Unreleased changelog](CHANGELOG.md) when the change affects users.

Keep dependency updates, lifecycle changes, documentation, and release infrastructure reviewable. Use focused conventional commit subjects such as `fix: preserve enabled state when stopping` or `docs: explain manual recovery`. A PR or green build alone is not release approval; never create a public release as a side effect of tests.
