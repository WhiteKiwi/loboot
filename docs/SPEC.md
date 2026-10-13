# loboot v0.1 specification

A single Rust binary manages explicitly owned native user services. No daemon, scheduler, database, root/system scope, secret storage, or Windows emulation.

## Contract

- TOML specifies a safe ID, absolute executable and working directory, argv, once/service mode and never/on-failure/always restart policy.
- Definition installation does not start or enable a service. Runtime actions are distinct from login persistence.
- Mutations first produce a reviewable plan. Dry-run performs no filesystem writes, locks, or service-manager calls.
- Existing foreign or modified definitions, symlinks, traversal and invalid manifests fail closed. External discovery is read-only.
- Human output and a versioned JSON envelope expose errors and partial failures honestly. A process exit is not inferred from a command being accepted.
- On Linux user systemd supports runtime and login persistence independently. On macOS launchd supports LaunchAgents login persistence and explicit runtime jobs. Start/restart requires prior enable; disable requires an unloaded job. These prerequisite errors preserve honest separation rather than temporarily changing persistence behind the user’s back.
- Repeated actions must be safe. Interrupted multi-resource updates must leave diagnostic evidence and refuse further mutations until explicitly recovered.

## Verification

Pure rendering, escaping, planner, ownership, interrupted-state and fake service-manager tests run without changing any real service. Native lifecycle verification is separate and opt-in in an isolated disposable login session. No real host lifecycle is claimed until that verification runs.

## Deferred

Boot/system services; Windows; environment/secret features; GUI; import/adopt of foreign services; automatic repair of externally changed state.

## Distribution scope

The owner approved MIT OR Apache-2.0 licensing and an experimental v0.1.0 distribution through GitHub release archives and Homebrew, with assistant integrations maintained in WhiteKiwi/skills. Binary packaging covers macOS/Linux x64 and ARM64 only after their native build lanes pass. Signed/notarized macOS binaries, crates.io, and registry credentials are separate decisions. A binary installation must not change any service state. Real logout/login behavior is not claimed without dedicated evidence.
