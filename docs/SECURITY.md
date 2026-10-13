# Security and trust boundaries

loboot is an early, unprivileged user-service manager. Full logout/login acceptance and a security audit remain pending. Source availability and passing tests are not security certifications; the project license is MIT OR Apache-2.0.

## What is guarded

- Only records with the expected schema, matching ID/native name, renderer output, and exact native-file bytes are considered managed.
- Foreign definitions, changed generated files, systemd drop-ins, unexpected native source paths, and name collisions refuse mutation. A matching filename prefix is not ownership.
- Managed access uses descriptor-relative operations, no-follow opens, regular-file checks, restrictive creation modes, and atomic replacement of each individual file. Symlink ancestors and hardlinked managed files are refused.
- A mutation lock coordinates loboot invocations. Snapshots are rechecked before filesystem changes. A journal preserves before/after evidence if a multi-file change is interrupted.
- Native commands use an executable plus argv, not a composed shell command. systemd escaping and launchd XML encoding have separate implementations and tests.
- Dry-run never invokes the native manager or changes files. Offline `--root` mode never executes manager commands.

These are defensive checks, not a guarantee of cross-filesystem atomicity or a security boundary against the account owner.

## What you must trust

The user account, administrators, chosen programs, working directories, and native service manager are trusted. Any program you install can read or change data available to your account. loboot does not sandbox it, restrict its network access, or inspect its behavior. Passing a shell executable explicitly can run shell code even though loboot itself never interprets shell syntax.

On Linux, `systemctl` is resolved through `PATH`; use a trusted command search path. macOS invokes `/bin/launchctl` directly. No root/system scope, user switching, or systemd linger changes are implemented.

Advisory locks serialize cooperating loboot commands only. External native tools and other processes can change state independently. loboot detects many such conflicts but does not take control of the whole login session or automatically undo native actions.

## Plaintext data and reports

Manifests, argv, paths, rendered definitions, JSON output, and recovery journals can contain ordinary application information in plaintext. There is no secret store, environment injection, credential redaction guarantee, or encrypted state feature. Do not put passwords, tokens, or other credentials in arguments or manifests.

Before sharing a bug report, inspect and redact output from `show`, `explain`, `doctor`, `status`, native stderr, or a journal. Preserve enough non-sensitive path/argv structure to reproduce the issue. Never attach an entire home directory or unreviewed environment dump.

## Reporting a concern

No security response SLA or confirmed private disclosure channel is established in this repository yet. Do not post exploit details or secrets publicly. If GitHub's repository Security tab offers private vulnerability reporting, use that verified option; otherwise first ask the maintainer for a private contact without disclosing the vulnerability publicly.

Useful evidence includes the exact source revision, `loboot --version`, OS/architecture, a minimal sanitized manifest, expected versus observed behavior, and the trust boundary crossed. Keep original journal/definition evidence privately when it concerns recovery or ownership.

## Distribution boundary

Source builds, experimental GitHub release archives, and the WhiteKiwi Homebrew formula are described in [installation](INSTALL.md). Packaging rehearsal artifacts are not a published release. SHA-256 checksums detect changed bytes; they are not independent publisher authentication. macOS archives are unsigned/unnotarized. Publication requires the approved version, exact-revision checks, license notices, and gates in [RELEASE.md](RELEASE.md). Do not bypass OS executable protections to try an unverified artifact.
