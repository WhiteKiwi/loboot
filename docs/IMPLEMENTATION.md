# Implementation and verification

## Sequence and acceptance

1. Specify lifecycle and ownership semantics before effects. Verify: documented native command matrix and explicit unsupported boundaries.
2. Implement pure manifest validation/rendering and argv planning. Verify: escaping corpus, malformed IDs/paths, policy combinations and command-order tests.
3. Implement owned state and journaled changes. Verify: collision/drift/symlink refusal, identical retries, stale-plan rejection, interrupted-state blockers and locked mutation checks.
4. Integrate CLI and safe offline mode. Verify: subprocess tests snapshot filesystem before/after every dry-run; an empty PATH and offline mode prohibit manager execution.
5. Format, lint and test on Linux/macOS CI. Verify exact remote commit checks; distinguish local mocks from native lifecycle evidence.
6. Perform real lifecycle testing only after explicit authorization in an isolated disposable user session. Real logout/login acceptance remains pending. Ordinary tests never perform native effects; a separately opted-in hosted release-readiness fixture records lifecycle-only evidence.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Ordinary tests never register, stop, start, enable or disable a real host service. The Runner fake verifies native command sequences and errors; offline CLI tests verify state. Ordinary CI runs the same safe suite on Ubuntu and macOS. The release-readiness workflow additionally opts into a guarded disposable-runner lifecycle fixture.

## Opt-in isolated native validation protocol (pending)

`scripts/native_smoke.py` implements lifecycle-only checks for disposable GitHub-hosted runners. It requires `--allow-hosted-native`, the hosted-runner environment, a non-root account with its real HOME, and no pre-existing loboot state. It never creates a manager, changes lingering, logs out, or uses the user’s computer. Exit 77 plus JSON evidence means skipped because an existing manager/session was unavailable; it is not a pass. All created labels are unique, owned resources are cleaned up, and unrelated native files are compared before/after. macOS retains only the unique disabled-label overrides until runner disposal because launchctl has no safe per-label erase.

Invocation: `python3 scripts/native_smoke.py --binary /absolute/loboot --allow-hosted-native --report /absolute/evidence.json`. The packaging workflow preserves this evidence even on failure. Read the result, checked cases, cleanup errors, and limitations before making any native support claim. The longer manual logout/login acceptance protocol below remains pending.

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

Real logout/login activation evidence; automated explicit recovery; logs command; richer read-only discovery; Windows/system scope. These are visible limitations rather than placeholder success responses.

## Recorded verification evidence

Cloud Linux, Rust 1.99.0: formatting check and Clippy with warnings denied pass; 46 tests pass (37 library, 2 CLI-unit, 7 subprocess integration). These tests include both native planners through fakes and Linux/macOS offline CLI flows. A generated macOS plist also parses with Python's plistlib and preserves literal argv.

An additional systemd-analyze 257 syntax-check attempt could not run because its verification setup tried to create `/run/systemd` on a read-only filesystem. It did not register or run a service. This is a blocked check, not a pass. This historical local restriction does not imply that hosted tests failed. Real native lifecycle tests remain pending; no unit, offline, packaging, or hosted compilation result establishes native login behavior.

### Hosted implementation checks

The initial implementation PR head `e9f03c9` passed Linux/macOS formatting, Clippy, and the 46-test suite in [run 38371124741](https://github.com/WhiteKiwi/loboot/actions/runs/38371124741). It merged as `0af5a30`; that exact main revision also passed [run 38371400030](https://github.com/WhiteKiwi/loboot/actions/runs/38371400030). These are historical results for those revisions, not evidence for later changes.

An independent review also completed 20 additional offline CLI probes. Those probes exercised explicit sandbox behavior and refusal paths; they did not contact real native managers.

Subsequent documentation/release infrastructure uses the checks described in [RELEASE.md](RELEASE.md). Verify the current commit's workflow results before merging or releasing; a workflow definition and a green previous revision are insufficient.

### v0.1.0 distribution plan

1. Apply the owner-selected dual license and include dependency/Rust runtime notices. Verify: Cargo metadata and nonempty upstream notice files are checked during packaging.
2. Package four native targets and generate a binary-only Homebrew formula from the combined manifest. Verify: architecture headers, extracted-binary smoke, exact inventory, and checksums on all four hosted lanes.
3. Exercise an explicitly opted-in disposable hosted lifecycle fixture separately from ordinary tests. Verify: exact command evidence, guarded scope, cleanup, and an explicit skipped result for missing sessions; no logout/login claim.
4. Publish only the approved version tag after exact-revision checks. Verify: downloaded release assets and hashes, followed by tap installation/tests.

Dependency review found MIT-compatible choices throughout the locked graph, with the additional Unicode-3.0 notice for unicode-ident. Packaging includes target-filtered build/dev dependencies conservatively, their upstream license/notice texts, and the pinned Rust toolchain’s standard-library copyright inventory. This is a recorded dependency inventory review, not a blanket legal warranty.

### First hosted native rehearsal

At PR #5 head `3fec464`, both Linux x64 and ARM64 lifecycle fixtures passed, including once/service/failure cases and cleanup ([run 38419342509](https://github.com/WhiteKiwi/loboot/actions/runs/38419342509)). Both macOS lanes exposed a real disabled-override parser incompatibility after installation. Modern launchctl reports `enabled`/`disabled`, while the original parser expected only legacy booleans; [the macOS Security Compliance Project recorded this format change](https://github.com/usnistgov/macos_security/issues/167). This failed run was rejected before publication. The parser fix must pass all native packaging lanes before release. Results are revision-specific; consult the final release workflow for exact tagged-revision evidence.
