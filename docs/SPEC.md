# loboot v0.1 specification

A single Rust binary manages explicitly owned native user services. No daemon, scheduler, database, root/system scope, secret storage, or Windows emulation.

## Contract

- TOML specifies six required fields: a safe ID, absolute executable and working directory, argv, once/service mode and never/on-failure/always restart policy. Unreleased source support adds only an optional literal `[environment]` string map; it does not loosen the required fields or shell-free execution contract.
- Definition installation does not start or enable a service. Runtime actions are distinct from login persistence.
- Mutations first produce a reviewable plan. Dry-run performs no filesystem writes, locks, or service-manager calls.
- Existing foreign or modified definitions, symlinks, traversal and invalid manifests fail closed. External discovery is read-only.
- Human output and a versioned JSON envelope expose errors and partial failures honestly. A process exit is not inferred from a command being accepted.
- On Linux user systemd supports runtime and login persistence independently. On macOS launchd supports LaunchAgents login persistence and explicit runtime jobs. Start/restart requires prior enable; disable requires an unloaded job. These prerequisite errors preserve honest separation rather than temporarily changing persistence behind the user’s back.
- Repeated actions must be safe. Interrupted multi-resource updates must leave diagnostic evidence and refuse further mutations until explicitly recovered.

## Environment contract (unreleased)

Environment configuration is per service, not a secret store or shell activation mechanism. Allow at most 64 entries, sorted by name: 1–128 byte ASCII names matching `[A-Za-z_][A-Za-z0-9_]*`, and 0–4096 UTF-8 byte string values without controls or Unicode noncharacters. Limit the total to 32768 bytes, summing name bytes, value bytes, and two separator/terminator bytes per entry.

Absent `PATH` preserves native-manager defaults. Explicit `PATH` completely overrides it with a nonempty colon-separated list of absolute directory paths, without empty, relative, dot, or traversal entries; directory existence is not checked. Never snapshot/import the invoking shell environment or mutate global manager environment. Executable and argv remain absolute/literal as before, with no variable/tilde expansion or virtualenv activation.

Store exact original values and native bytes privately as plaintext. Ordinary inspection and plan/apply/dry-run summaries redact configured values through public projections while retaining names; native-definition previews with redactions are marked non-replayable. Validation errors must not quote the rejected values. The explicit `recover plan --json` export is sensitive and retains raw values so apply can verify equality, even with `--dry-run`; it is not a shareable diagnostic report. Human recovery-plan output is a value-free summary. Native managers, child processes, native tools, and application output may expose values. No sandbox, encryption, or credential safety is promised.

Omitted/empty environments preserve existing exact rendering and readable schema-1/schema-2 records. Older binaries reject new nonempty environment records; the feature is independent of schema-2 log capture. Published v0.1.0 is unchanged and does not support environment configuration. [CLI limits and operator workflow](CLI.md#environment-and-path-unreleased).

## Verification

Pure rendering, escaping, planner, ownership, interrupted-state and fake service-manager tests run without changing any real service. Native lifecycle verification is separate and opt-in in an isolated disposable login session. No real host lifecycle is claimed until that verification runs.

## Deferred

Boot/system services; Windows; secret storage/injection; shell-environment import and virtualenv activation; GUI; import/adopt of foreign services; automatic repair of externally changed state.

## Distribution scope

The owner approved MIT OR Apache-2.0 licensing and an experimental v0.1.0 distribution through GitHub release archives and Homebrew, with assistant integrations maintained in WhiteKiwi/skills. Binary packaging covers macOS/Linux x64 and ARM64 only after their native build lanes pass. Signed/notarized macOS binaries, crates.io, and registry credentials are separate decisions. A binary installation must not change any service state. Real logout/login behavior is not claimed without dedicated evidence.
