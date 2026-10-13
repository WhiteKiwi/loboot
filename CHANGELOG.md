# Changelog

This file records user-facing changes. Versioned entries supply the curated GitHub release notes.

## Unreleased

### Added

- Bounded Linux user-journal and private macOS stdout/stderr reads, fixed failure diagnostics, and byte/deadline-limited native helper capture. Legacy records remain readable; explicit stopped update opts macOS jobs into schema-2 capture. Application logs remain potentially sensitive and active file growth is not automatically capped.
- Explicit isolated-account logout/login acceptance protocol, scoped invocation fixture, and bounded offline evidence validation. Synthetic tests verify report consistency only; real Linux/macOS session acceptance remains pending.

### Fixed

- Omit redundant Homebrew version metadata; Homebrew infers the version from all four archive URLs. The v0.1.0 tap formula applies the same one-line correction without changing any published release asset.

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
- Homebrew formula generation from the verified release manifest; the tap owns periodic updates using its own ephemeral token, without a cross-repository credential. No `brew services` integration is installed.
- Separate [WhiteKiwi/skills integration](https://github.com/WhiteKiwi/skills) for assistant instructions; installing a skill does not install the CLI.

### Changed

- Align release automation with locron: version-tag pushes trigger publication automatically, and curated tagged changelog entries supply release notes. Main-branch merges do not release.

### Fixed

- Parse modern macOS launchctl `enabled`/`disabled` overrides as well as legacy `false`/`true`, while refusing unknown values. The first real hosted lifecycle test caught this before v0.1.0 publication.

- Wait for launchd teardown after bootout before completing stop or reloading during restart; revalidate ownership/persistence and never blindly retry a bootstrap.

### Known limits

- Real logout/login acceptance remains pending. Disposable hosted lifecycle checks report their own results separately from ordinary offline tests.
- Experimental first release: lifecycle/login acceptance remains restricted to the evidence reported in CI; no real logout/login acceptance is claimed. macOS archives are unsigned/unnotarized; Linux binaries target Ubuntu 24.04/glibc 2.39 or newer.
- No crates.io publication is included. Assistant skill registry distribution is managed separately in WhiteKiwi/skills. Installing the binary never enables or starts a service.
- Built-in logs, secrets/environment configuration, automatic recovery, Windows, system/boot scope, and external-service adoption are deferred.
