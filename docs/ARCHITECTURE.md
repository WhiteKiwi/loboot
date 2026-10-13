# Architecture

One Rust package contains a library for testing and one user-facing binary. No background process or database.

## Boundaries

1. `model`: strict TOML and syntax/filesystem validation; no shell language.
2. `backend`: native rendering, exact argv command plans, status parsing and a `Runner` port. Real subprocesses and scripted test runners implement the same boundary.
3. `store`: managed manifest records, exact expected native bytes, nonmutating plans, descriptor-relative filesystem access, locking and recovery journals.
4. CLI: dispatch, preconditions, structured output, offline mode and native execution.

The reference is locron's small ServicePort and fake-backend pattern at [a3f43fc](https://github.com/WhiteKiwi/locron/blob/a3f43fc15e1a5db75d811e65e35add59d51c055a/crates/locron-cli/src/service.rs). loboot does not import locron's daemon-specific enable/start coupling, SIGTERM-only stop, or unit-string escaping. It is a separate product and package.

## Managed state

Per-service records are TOML with a `[manifest]` table and exact-render metadata. Default records are under `~/.local/share/loboot`; generated Linux definitions are under `~/.config/systemd/user`, macOS definitions under `~/Library/LaunchAgents`. Offline roots have separate `state/` and `native/` directories. Records carry the manifest and the exact expected rendered bytes, not just a recognizable filename prefix. A record is valid only if its ID, schema, renderer output, and native file all agree. Legacy schema-1 renderings remain byte-for-byte unchanged. New macOS schema-2 records contain a validated random log generation; capture paths are derived from that generation and the configured store, never accepted as arbitrary stored paths.

A pure plan reads current state, validates ownership and collision rules, and lists changes. Applying acquires an exclusive advisory lock and rechecks the expected snapshot before modifying anything. Native lifecycle operations share this lock with filesystem changes. Locks serialize cooperating loboot invocations; they cannot control external tools or launchd/systemd itself. External administrative modifications remain an explicit drift/error condition, not permission to overwrite.

Managed filesystem access uses descriptor-relative `openat` with `O_NOFOLLOW`, regular-file checks, atomic same-directory rename and restrictive creation modes. Symlink ancestors are refused rather than followed. The user account itself and administrators remain trusted; this is not a security boundary against another process already acting as that user.

## Native execution

Every command is an executable plus argv, never a shell string. systemd quoting and launchd XML encoding are separate. Dry-run never uses Runner; unknown loaded state becomes conditional plans. Actual execution checks manager availability and current native state, revalidates ownership, executes the selected plan, then reads native state again.

macOS add disables a fresh label before exposing its LaunchAgent plist. The generated plist is disabled by default; explicit enable overrides that preference. Loaded disabled-start/restart and loaded disable are rejected rather than implemented through surprising temporary state changes. A failed filesystem commit after disabling a fresh label can leave that label disabled; the error and journal must be inspected. No process is started by this preparatory step.

## Recovery

Two paths cannot be atomically committed together. Before native/record writes, store writes `.pending.json` with before and after records and exact bytes. Each file replacement is atomic; completion removes the journal after both sides agree. A crash or I/O failure preserves the journal and blocks further management.

For recovery, stop and inspect native state using the platform tools. Back up the state directory and relevant definition first. Inspect the journal's explicit paths and before/after records. Either restore the before pair or complete the after pair (a null record means the corresponding files should be absent). Verify native bytes and record agree, then remove the journal and run `loboot doctor`. Do not simply delete the journal to silence the error. Automated recovery is deliberately deferred. Follow the detailed [manual recovery procedure](TROUBLESHOOTING.md#interrupted-filesystem-change), including account/path verification and preserving evidence before changing files.

Native service-manager operations cannot be rolled back transactionally. Errors report the completed command prefix; final-query failures explicitly say native commands may have succeeded. Rerun status before retrying. The filesystem journal does not claim to journal OS runtime state.

## Threat and capability limits

No secrets feature; argv and paths are ordinary plaintext. No arbitrary shell evaluation, external service adoption, symlink repair, system scope, root, account changes, or lingering changes. Persistent login preference and runtime state are reported separately. Real native timing and GUI-login behavior require isolated host validation; mocks cannot establish those facts.

## Log capture boundary (unreleased)

`logs` implements bounded text sampling/redaction; `process` implements nonblocking, deadline- and byte-limited capture for native helper processes. State parsers refuse incomplete capture and retain only known bounded scalar keys. Ordinary native errors expose fixed hints plus exit/signal evidence, not arbitrary stderr.

macOS capture generations live below a private marked log root. UID, exact private modes, regular-file/nlink checks, no-follow descriptors, exclusive creation, root/generation markers, and derived paths prevent accidental adoption or truncation. New generation initialization follows journal creation and precedes native definition publication. A partial initialization leaves evidence; recovery must not silently delete it. Runtime enable/start/restart validates capture, and restart rechecks after teardown. These checks do not sandbox another process already acting as the same trusted account, nor control launchd's later file opens at login.

Old records are migrated only by explicit stopped update. Definitions and capture are independent of retention: remove retains logs; ID reuse gets a new generation. Manual stopped/unloaded rotation is documented; no daemon or global journal-retention change is introduced.
