# Implementation and verification

## Sequence and acceptance

1. Specify lifecycle and ownership semantics before effects. Verify: documented native command matrix and explicit unsupported boundaries.
2. Implement pure manifest validation/rendering and argv planning. Verify: escaping corpus, malformed IDs/paths, policy combinations and command-order tests.
3. Implement owned state and journaled changes. Verify: collision/drift/symlink refusal, identical retries, stale-plan rejection, interrupted-state blockers and locked mutation checks.
4. Integrate CLI and safe offline mode. Verify: subprocess tests snapshot filesystem before/after every dry-run; an empty PATH and offline mode prohibit manager execution.
5. Format, lint and test on Linux/macOS CI. Verify exact remote commit checks; distinguish local mocks from native lifecycle evidence.
6. Perform real lifecycle testing only after explicit authorization in an isolated disposable user session. This is pending and not performed by ordinary tests or CI.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Ordinary tests never register, stop, start, enable or disable a real host service. The Runner fake verifies native command sequences and errors; offline CLI tests verify state. CI runs the same safe suite on Ubuntu and macOS, not real services.

## Opt-in isolated native validation protocol (pending)

This is a manual protocol, not an automatically executed test. Obtain authorization first; use a disposable logged-in account/session with no matching service labels. Never point it at an existing user job. Use a unique `loboot-e2e-<suffix>` ID and an absolute test executable that records invocations in an isolated directory.

1. Capture initial native state and relevant paths. Add and verify zero runs and disabled persistence.
2. Enable and verify zero immediate runs. Start once and verify exactly one invocation. Verify observed native fields without assuming long-lived running state.
3. Start a service fixture, then stop and verify no KeepAlive respawn while enabled preference remains unchanged.
4. Disable and verify no future activation. On macOS verify loaded-disable and disabled-start errors leave state unchanged.
5. Test restart, stopped-definition update, missing session, one injected failing executable, and repeated commands.
6. With separate consent for logout/login, verify login activation. Linux tests must account for user-manager lifetime and linger without changing linger settings.
7. Stop, disable, remove only the uniquely created test resources; verify original state is unchanged and retain evidence.

No native end-to-end claim can be inferred from unit tests or a macOS runner merely compiling the project.

## Deferred work

Native lifecycle evidence; automated explicit recovery; logs command; richer read-only discovery; Windows/system scope; license decision. These are visible limitations rather than placeholder success responses.

## Recorded verification evidence

Cloud Linux, Rust 1.99.0: formatting check and Clippy with warnings denied pass; 46 tests pass (37 library, 2 CLI-unit, 7 subprocess integration). These tests include both native planners through fakes and Linux/macOS offline CLI flows. A generated macOS plist also parses with Python's plistlib and preserves literal argv.

An additional systemd-analyze 257 syntax-check attempt could not run because its verification setup tried to create `/run/systemd` on a read-only filesystem. It did not register or run a service. This is a blocked check, not a pass. This historical local restriction does not imply that hosted tests failed. Real native lifecycle tests remain pending; no unit, offline, packaging, or hosted compilation result establishes native login behavior.

### Hosted implementation checks

The initial implementation PR head `e9f03c9` passed Linux/macOS formatting, Clippy, and the 46-test suite in [run 38371124741](https://github.com/WhiteKiwi/loboot/actions/runs/38371124741). It merged as `0af5a30`; that exact main revision also passed [run 38371400030](https://github.com/WhiteKiwi/loboot/actions/runs/38371400030). These are historical results for those revisions, not evidence for later changes.

An independent review also completed 20 additional offline CLI probes. Those probes exercised explicit sandbox behavior and refusal paths; they did not contact real native managers.

Subsequent documentation/release infrastructure uses the checks described in [RELEASE.md](RELEASE.md). Verify the current commit's workflow results before merging or releasing; a workflow definition and a green previous revision are insufficient.
