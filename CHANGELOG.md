# Changelog

This file records user-facing changes. Versioned entries supply the curated GitHub release notes.

## Unreleased

## [0.1.0] - 2026-10-13

### Added

- Native user-service CLI for Linux systemd and macOS launchd, with explicit definition, runtime, and future-activation operations.
- Direct argv and strict TOML manifests, `once`/`service` modes, and native restart policies.
- Offline roots, non-mutating dry-run plans, and versioned `loboot.cli/v1` JSON.
- Exact ownership records, drift/collision refusal, guarded filesystem access, mutation locking, and interrupted-change journals.
- Read-only discovery and state/session diagnostics.
- Linux/macOS fake-runner and offline CLI tests, pinned lint tooling, source/operator/security/recovery documentation, and guarded release infrastructure.

- Owner-approved MIT OR Apache-2.0 licensing, including license texts and third-party/Rust runtime notices in binary archives.
- Native x64/ARM64 macOS and Linux archive matrix with architecture/version checks, exact inventory, SHA-256 checksums, and downloaded-binary smoke validation.
- Homebrew formula generation from the verified release manifest; the tap owns token-free periodic updates. No `brew services` integration is installed.
- Separate [WhiteKiwi/skills integration](https://github.com/WhiteKiwi/skills) for assistant instructions; installing a skill does not install the CLI.

### Changed

- Align release automation with locron: version-tag pushes trigger publication automatically, and curated tagged changelog entries supply release notes. Main-branch merges do not release.

### Known limits

- Real host lifecycle/login acceptance remains pending; CI does not operate real services.
- Experimental first release: lifecycle/login acceptance remains restricted to the evidence reported in CI; no real logout/login acceptance is claimed. macOS archives are unsigned/unnotarized; Linux binaries target Ubuntu 24.04/glibc 2.39 or newer.
- No crates.io or ClawHub publication is included. Installing the binary never enables or starts a service.
- Built-in logs, secrets/environment configuration, automatic recovery, Windows, system/boot scope, and external-service adoption are deferred.
