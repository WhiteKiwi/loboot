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

## Opt-in isolated native lifecycle validation

`scripts/native_smoke.py` implements lifecycle-only checks for disposable GitHub-hosted runners. It requires `--allow-hosted-native`, the hosted-runner environment, a non-root account with its real HOME, and no pre-existing loboot state. It never creates a manager, changes lingering, logs out, or uses the user’s computer. Exit 77 plus JSON evidence means skipped because an existing manager/session was unavailable; it is not a pass. All created labels are unique, owned resources are cleaned up, and unrelated native files are compared before/after. macOS retains only the unique disabled-label overrides until runner disposal because launchctl has no safe per-label erase.

Invocation: `python3 scripts/native_smoke.py --binary /absolute/loboot --allow-hosted-native --report /absolute/evidence.json`. The packaging workflow preserves this evidence even on failure. Read the result, checked cases, cleanup errors, and limitations before making any native support claim. The longer manual logout/login acceptance protocol below remains pending.

### Manual logout/login acceptance (pending)

Use the [dedicated logout/login protocol](LOGIN-ACCEPTANCE.md), offline evidence template/validator, and scoped invocation fixture. The protocol requires separately authorized isolated sessions, an independent observer of actual manager/GUI-domain absence, enabled and disabled login cycles, and exact owned cleanup. A consistent operator report is not independent proof; original native evidence still needs human review.

The new preparation tools and their synthetic tests do not change acceptance status. [Issue #9](https://github.com/WhiteKiwi/loboot/issues/9) remains open until actual Linux and macOS session evidence is recorded. No user session was logged out for this work.

## Deferred work

Real logout/login activation evidence; richer read-only discovery; Windows/system scope. Explicit guarded recovery is described below; automatic repair or foreign-state adoption remains out of scope. These are visible limitations rather than placeholder success responses.

## Explicit recovery contract and acceptance (unreleased)

[Issue #11](https://github.com/WhiteKiwi/loboot/issues/11) adds operator-selected filesystem recovery through `recover plan --to before|after --json` (default `before`) and `recover apply PLAN.json`. Planning snapshots raw before/after definitions, journal/current state and file metadata without effects. Apply re-reads under the exclusive mutation lock, rejects a stale or changed plan, and retains the original journal until the selected pair is verified. It performs no native mutations or capture cleanup.

Acceptance coverage must distinguish these boundaries:

- Before/after recovery for interrupted add, update and removal, including mixed record/native pairs and supported record schemas 1 and 2.
- Exact snapshot revalidation; unknown/malformed journal or plan, foreign/drifted bytes, root/platform mismatch, unsafe symlinks/hardlinks and metadata changes must refuse without unsafe writes.
- Live fake-runner checks for Linux not running or macOS unloaded, and safe disabled/no-activation state for absent targets. Unknown state must refuse; no stop/start/enable/disable/daemon-reload is permitted.
- Filesystem snapshots around plan, dry-run apply, no-pending planning and completed-plan replay. Dry-run has no writes, locks or native queries; offline `--root` has no native calls. Successful recovery is followed by `pending: false`; completed replay returns `no_pending_recovery` with no changes.
- Partial recovery keeps the original journal and requires a fresh plan. Schema-2 targets require complete verified capture; rollback before incomplete log initialization can retain schema-1/absent targets and log remnants without adoption, deletion, truncation or initialization.
- CLI JSON purity, private plan-file guidance, and the distinction between restoring files and native runtime/preferences.

This section records the implementation contract and required acceptance scope, **not results of an executed check**. Exact-revision results belong with the change's validation evidence. Historical v0.1.0 checks below do not verify these unreleased commands; native logout/login acceptance remains pending.

## Environment contract and acceptance (unreleased)

[Issue #12](https://github.com/WhiteKiwi/loboot/issues/12) adds optional bounded per-service environment strings and explicit complete `PATH` overrides. The required acceptance scope is:

- Deterministic sorted literal native rendering on Linux/macOS, including spaces, quotes, backslashes, dollar/percent signs, equals signs, empty values and non-ASCII UTF-8. Configured values must not trigger variable/specifier/shell expansion.
- Exact name/value/count/aggregate boundaries, control and Unicode-noncharacter rejection, non-string TOML rejection, and syntax-only `PATH` rules without ambient shell import or manager-global changes.
- Omitted/empty-map byte compatibility for old definitions and stored schemas 1/2; explicit stopped updates for configuration changes. Schema-2 capture remains separate.
- Redacted public list/show/explain/native previews and ordinary plan/apply/dry-run/error output, while stored native/record/journal evidence remains exact. Recovery exports retain sensitive raw values for equality checks; redaction is not a secret-store claim.
- Read-only/offline refusal and success paths, pure-renderer/fake-runner checks, and subprocess output-leakage regressions. No default test may call a real native manager or register a service.

This is the feature contract and required acceptance scope, **not an assertion that these checks ran or passed**. Revision-specific local/hosted results must be recorded separately. Existing v0.1.0 lifecycle evidence does not establish native environment behavior, and real logout/login acceptance remains pending.

## Recorded verification evidence

Cloud Linux, Rust 1.99.0: formatting check and Clippy with warnings denied pass; 46 tests pass (37 library, 2 CLI-unit, 7 subprocess integration). These tests include both native planners through fakes and Linux/macOS offline CLI flows. A generated macOS plist also parses with Python's plistlib and preserves literal argv.

An additional systemd-analyze 257 syntax-check attempt could not run because its verification setup tried to create `/run/systemd` on a read-only filesystem. It did not register or run a service. This is a blocked check, not a pass. This historical local restriction does not imply that hosted tests failed. Native lifecycle tests were pending at that initial revision. The v0.1.0 hosted lifecycle results below supersede that status; no unit, offline, packaging, or hosted compilation result establishes native login behavior.

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

At PR #5 head `3fec464`, both Linux x64 and ARM64 lifecycle fixtures passed, including once/service/failure cases and cleanup ([run 38419342509](https://github.com/WhiteKiwi/loboot/actions/runs/38419342509)). Both macOS lanes exposed a real disabled-override parser incompatibility after installation. Modern launchctl reports `enabled`/`disabled`, while the original parser expected only legacy booleans; [the macOS Security Compliance Project recorded this format change](https://github.com/usnistgov/macos_security/issues/167). This failed run was rejected before publication. The parser fix subsequently passed all four native packaging lanes; the final tagged-revision evidence is recorded below. Results are revision-specific; consult the final release workflow for exact tagged-revision evidence.

### Native teardown synchronization fix

The first publication attempt ([run 38422058226](https://github.com/WhiteKiwi/loboot/actions/runs/38422058226), commit `78c8c7b`) was stopped before creating a tag: macOS ARM intermittently rejected bootstrap immediately after a successful bootout during restart. Cleanup completed with no errors. This is a timing-sensitive lifecycle defect, not permission to bypass acceptance.

Plan before implementation:

1. After successful macOS bootout, retain the mutation lock and poll read-only observations for both an unloaded owned label and disappearance of all observed previous PIDs, with two consecutive settled observations and a 100-observation budget (99 pauses of 100 ms, plus native-query time; not a hard wall-clock deadline). Verify: fake-runner tests cover delayed unload, delayed process exit, unknown state, timeout, and no follow-on bootstrap before both conditions.
2. Revalidate stored/native ownership and enabled preference before the single restart bootstrap. Never retry bootstrap/kickstart automatically. A query/ownership/persistence change or timeout reports partial progress and refuses the next mutating command. Verify: regression tests assert exactly one bootstrap and zero bootstrap on uncertain/changed state.
3. Apply the same settlement boundary to stop so a subsequent update/start does not race teardown. Strengthen the isolated hosted fixture with delayed graceful exit and repeated restart cycles. Verify: all four platform lanes pass lifecycle and exact cleanup, with Linux behavior unchanged.
4. Merge the reviewed fix only after exact-head CI/readiness, then repeat the same approved v0.1.0 release request at the new exact main commit. No existing tag or release bytes will be moved.

The teardown rationale is supported by [Apple’s historical launchd source](https://github.com/apple-oss-distributions/launchd/blob/main/src/core.c), which tracks `removal_pending` for running jobs until exit. That published implementation supports investigating asynchronous removal; it is not a guarantee of current launchd internals. Current hosted-runner evidence remains the acceptance test.

### Published v0.1.0 evidence

[v0.1.0](https://github.com/WhiteKiwi/loboot/releases/tag/v0.1.0) was published on 2026-10-13 from `207bc318ffbcb78883d1c32f668489799060c429`, after [exact-head PR readiness](https://github.com/WhiteKiwi/loboot/actions/runs/38425247370) and [merged-main CI](https://github.com/WhiteKiwi/loboot/actions/runs/38425998074) passed.

The [successful release run](https://github.com/WhiteKiwi/loboot/actions/runs/38426791733) reran the full gates and all four native lanes: Linux x64/ARM64 and macOS x64/ARM64. Each lifecycle report recorded `passed`, three delayed-exit restart cycles without overlapping previous PIDs, and an empty cleanup-error list. No native lane was skipped. These are disposable hosted-session lifecycle checks, not logout/login or boot activation evidence.

The revision passed 63 Rust tests (40 library, 16 CLI unit, 7 integration), 72 Python tests, formatting, warnings-denied Clippy, and offline documentation checks. The publication job verified downloaded release bytes. An independent public download then verified all seven GitHub asset digests, archive hashes against both manifest and checksum file, all four architecture headers and exact inventories, and byte-for-byte generated-formula agreement. A copied public Linux x64 binary passed version/help and an offline JSON dry-run with isolated HOME and empty PATH.

The [tap registration](https://github.com/WhiteKiwi/homebrew-tap/pull/3) merged after [exact-head Homebrew validation](https://github.com/WhiteKiwi/homebrew-tap/actions/runs/38428877889) passed on macOS ARM64 and Linux x64: native installation, bottle creation/reinstallation, strict linkage, online/new-formula audit, style, and offline formula tests. The tap also verified all four release archives and passed 27 updater tests. Its v0.1.0 formula omits only the redundant explicit version line described in [the release guide](RELEASE.md#homebrew-distribution); archive bytes and hashes are unchanged.

The independent [Loboot skill release](https://github.com/WhiteKiwi/skills/releases/tag/loboot-v0.1.0) supplies portable and client-specific instructions. CLI and skill publication do not establish real user logout/login behavior or confer permission to manage a user's services. macOS downloads remain unsigned/unnotarized; published v0.1.0 Linux binaries retain the Ubuntu 24.04/glibc 2.39 tested baseline. The [unpublished Linux candidate policy](RELEASE.md#linux-candidate-baseline-unreleased) targets 22.04/2.35 separately and requires new exact-candidate native evidence; it does not rewrite this historical verification.
