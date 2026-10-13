# Changelog

This file records user-facing changes. No public package release has been made; the Cargo version `0.1.0` identifies the initial implementation, not an announced distribution.

## Unreleased

### Added

- Native user-service CLI for Linux systemd and macOS launchd, with explicit definition, runtime, and future-activation operations.
- Direct argv and strict TOML manifests, `once`/`service` modes, and native restart policies.
- Offline roots, non-mutating dry-run plans, and versioned `loboot.cli/v1` JSON.
- Exact ownership records, drift/collision refusal, guarded filesystem access, mutation locking, and interrupted-change journals.
- Read-only discovery and state/session diagnostics.
- Linux/macOS fake-runner and offline CLI tests, pinned lint tooling, source/operator/security/recovery documentation, and guarded release infrastructure.

### Changed

- Align release automation with locron: version-tag pushes trigger publication automatically, and curated tagged changelog entries supply release notes. Main-branch merges do not release.

### Known limits

- Real host lifecycle/login acceptance remains pending; CI does not operate real services.
- License and public distribution decisions are pending. Source builds are the only documented installation method.
- Built-in logs, secrets/environment configuration, automatic recovery, Windows, system/boot scope, and external-service adoption are deferred.
