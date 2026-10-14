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

On Linux, `systemctl` is resolved through the environment of the loboot invocation; use a trusted command search path. A manifest's per-service `PATH` does not change that helper lookup or loboot's own process environment. macOS invokes `/bin/launchctl` directly. No root/system scope, user switching, or systemd linger changes are implemented.

Advisory locks serialize cooperating loboot commands only. External native tools and other processes can change state independently. loboot detects many such conflicts but does not take control of the whole login session or automatically undo native actions.

## Plaintext data and reports

Manifests, argv, paths, configured environment values, rendered definitions, recovery journals, and saved recovery plans can contain ordinary application information in plaintext. Unreleased environment support is for non-secret configuration only: it is not a secret store, encrypted state, credential-injection system, or sandbox. Exact values remain on disk even when a display redacts them. Do not put passwords, tokens, or other credentials in arguments or manifests.

Public `list`, `show`, and `explain` projections retain configured names but redact their values, including values inside displayed native definitions. Redacted definitions are explicitly non-replayable previews, not backups or installable definitions. Ordinary plan/apply/dry-run summaries and validation errors do not expose configured values; raw stored objects must not be substituted for those projections. This is targeted output hygiene, not comprehensive credential redaction: the same text can still appear in argv, paths, external native-tool output, or application logs. Log reads redact some common assignment/header forms, but unlabelled, split, encoded or unfamiliar application secrets can still appear.

The service manager, launched program, its descendants, the account owner, and administrators can access environment values. Native inspection tools and programs that print their environment can disclose them. A configured variable can change a program's behavior, library loading, or child-process command lookup. `PATH` validation only checks syntax, not directory existence, permissions, trust, or executable identity. Use trusted absolute directories; omitting `PATH` leaves platform/manager defaults rather than producing an empty or isolated environment. There is no shell-environment capture/import or global manager environment modification.

Before sharing a bug report, inspect and redact output from `show`, `explain`, `doctor`, `status`, native stderr, or a journal. Never share a raw recovery plan: it includes exact before/after definitions, configured environment values, and current filesystem evidence. Prepare a separate sanitized report instead; editing a plan is not supported. Preserve enough non-sensitive path/argv structure to reproduce the issue. Never attach an entire home directory or unreviewed environment dump.

## Recovery plans (unreleased)

`recover plan` is read-only and makes no native queries. Without `--json`, it shows a value-free summary. `recover plan --json` is an explicitly sensitive, exact plaintext export, including raw environment values in record/native evidence; `--dry-run` does not redact that export. It is not a secret-free support bundle, an authenticated backup, or permission to act on arbitrary paths. Redacting or editing the saved plan breaks the exact-equality evidence required by apply; produce a separate sanitized report for sharing instead. Write it to a new file in a private directory **outside the managed state/native trees**, with `umask 077` and mode 0600. The shell's redirect creates that file; `umask` does not fix existing permissions. Keep backups and log remnants private as well.

`recover apply` re-reads the journal/current state under the exclusive mutation lock and requires an exact fresh-plan match. It validates supported schemas, configured paths, ownership, definition bytes and file metadata; it refuses unknown, foreign, drifted, malformed, symlinked, hardlinked or stale evidence rather than adopting it. No force option exists. The original journal is removed only after the selected pair is verified; partial recovery retains evidence and requires a new plan. A completed replay refuses without changes.

Live application additionally uses read-only native checks for Linux not running or macOS unloaded, and safe disabled/no-activation state when the target is absence. Recovery makes no native mutations and does not restore manager runtime or preferences. Dry-run apply never writes, locks or makes native queries; offline recovery never contacts a manager. These checks still do not serialize external tools or another process acting as the same account.

Schema-2 targets require complete verified existing capture; recovery never initializes, adopts, deletes or truncates log remnants. Safe rollback to a schema-1/absent `before` target can leave interrupted capture remnants intact. Preserving those files is intentional, not a reason for recursive cleanup.

## Reporting a concern

No security response SLA or confirmed private disclosure channel is established in this repository yet. Do not post exploit details or secrets publicly. If GitHub's repository Security tab offers private vulnerability reporting, use that verified option; otherwise first ask the maintainer for a private contact without disclosing the vulnerability publicly.

Useful evidence includes the exact source revision, `loboot --version`, OS/architecture, a minimal sanitized manifest, expected versus observed behavior, and the trust boundary crossed. Keep original journal/definition evidence privately when it concerns recovery or ownership.

## Distribution boundary

Source builds, experimental GitHub release archives, and the WhiteKiwi Homebrew formula are described in [installation](INSTALL.md). Packaging rehearsal artifacts are not a published release. SHA-256 checksums detect changed bytes; they are not independent publisher authentication. macOS archives are unsigned/unnotarized. Publication requires the approved version, exact-revision checks, license notices, and gates in [RELEASE.md](RELEASE.md). Do not bypass OS executable protections to try an unverified artifact.

## Output capture (unreleased)

Native subprocess stdout/stderr capture is bounded and has an observation deadline. Failed/incomplete native operations can still have partial effects; never infer rollback from a timeout. Read-only status output uses allowlisted scalar fields and fixed hints. Log content is deliberately separate and remains potentially sensitive.

Private macOS captures require current effective UID ownership, 0700 directories and 0600 single-link regular files, with descriptor-relative no-follow access and exact ownership markers. Public or altered paths are refused rather than chmodded/repaired. The account owner and administrators remain trusted; later replacement by that same account, program-created descendants, and launchd's future path opens are outside a sandbox guarantee. Active files have no automatic disk-growth limit. Removing a service retains its capture evidence for deliberate cleanup.

Recovery additionally requires current-user state/native directories and evidence files without group/world-write permission, and trusted root/current-user ancestors. Root-owned sticky shared ancestors such as `/tmp` are allowed, but never symlinked components. This is not a sandbox against the account owner or administrators, and does not model ACLs.
