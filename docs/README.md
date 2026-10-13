# loboot documentation

Start with the [repository README](../README.md) for installation options and an offline trial.

## Using loboot

- [Installation](INSTALL.md): Homebrew, binary archives, source builds, updates, and removal.
- [Operator guide](OPERATOR.md): definitions, runtime versus persistence, status, and native paths.
- [CLI contract](CLI.md): commands, validation, dry-run, JSON, and exit behavior.
- [Troubleshooting](TROUBLESHOOTING.md): refusal codes, partial failures, and manual recovery.
- [Security](SECURITY.md): ownership checks, trust boundaries, and sensitive data.

## Developing and releasing

- [Contributing](../CONTRIBUTING.md): local checks and review expectations.
- [Specification](SPEC.md): product scope and behavior requirements.
- [Architecture](ARCHITECTURE.md): model/backend/store boundaries and filesystem design.
- [Implementation and verification](IMPLEMENTATION.md): test evidence and pending native acceptance.
- [Real login acceptance](LOGIN-ACCEPTANCE.md): isolated-session protocol, scoped fixture, and honest evidence checks.
- [Release process](RELEASE.md): CI, artifact rehearsal, and gates before distribution.
- [Changelog](../CHANGELOG.md): versioned release notes and upcoming changes.

Implementation, tested behavior, and release availability are separate claims. A green hosted build does not establish real login-session behavior or mean a package has been published.
