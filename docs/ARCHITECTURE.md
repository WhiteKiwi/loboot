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

## Environment representation and public projections (unreleased)

`Manifest` retains six required fields and adds an optional `BTreeMap<String, String>` environment map. A sorted map provides deterministic native rendering and equality. Empty maps are omitted from serialization, preserving the previous serialized record and native-renderer bytes for definitions without configured environment. Schema-1/schema-2 ownership validation still compares original records and native bytes; nonempty maps do not introduce a new schema, and schema 2 continues to describe log capture. Older binaries reject new nonempty environment records instead of silently dropping their configuration.

Syntax validation bounds entry count, ASCII names, UTF-8 value bytes, and the total `NAME=value` plus NUL budget before rendering. Controls and Unicode noncharacters are rejected. `PATH` has an additional syntactic absolute-directory-list check, not filesystem existence/trust checks. Neither model nor renderer imports ambient shell values. Linux renders explicit per-unit environment entries; macOS uses `EnvironmentVariables`. They preserve literal values using their own native encodings, independently of shell-free argv quoting. No process-wide or manager-global environment mutation is needed.

The persistence model is not the public output model. `list`, `show`, `explain`, and ordinary mutation/recovery apply/dry-run summaries use value-redacted projections. Names remain visible, and displayed native definitions containing redactions are marked non-replayable. Rendering a public preview must never overwrite the original stored bytes or turn placeholders into a new managed definition. Environment parse/validation errors must not copy raw values into error strings.

The deliberate exception is the sensitive exact `recover plan --json` export: recovery compares the unmodified before/after records and native bytes under lock, so its raw evidence must retain the original values. The human `recover plan` output is a value-free summary; `--json` opts into the raw export even with `--dry-run`. Generic summary/apply output must not serialize that raw recovery model by accident. [Limits](CLI.md#environment-and-path-unreleased) and [privacy boundary](SECURITY.md#plaintext-data-and-reports).

## Native execution

Every command is an executable plus argv, never a shell string. systemd quoting and launchd XML encoding are separate. Dry-run never uses Runner; unknown loaded state becomes conditional plans. Actual execution checks manager availability and current native state, revalidates ownership, executes the selected plan, then reads native state again.

macOS add disables a fresh label before exposing its LaunchAgent plist. The generated plist is disabled by default; explicit enable overrides that preference. Loaded disabled-start/restart and loaded disable are rejected rather than implemented through surprising temporary state changes. A failed filesystem commit after disabling a fresh label can leave that label disabled; the error and journal must be inspected. No process is started by this preparatory step.

## Recovery

Two paths cannot be atomically committed together. Before native/record writes, store writes `.pending.json` with before and after records and exact bytes. Each file replacement is atomic; completion removes the journal after both sides agree. A crash or I/O failure preserves the journal and blocks further management.

Source builds after v0.1.0 provide explicit, operator-selected recovery, not automatic repair. `recover plan --to before|after --json` reads the journal and current filesystem without writes, locks, or native queries. The default target is `before`. The sensitive exact plan export preserves before/after native definitions, raw configured environment values, journal/current-state evidence, and file metadata for private review. A null target record means the record and native definition must both be absent. Supported records use schema 1 or 2; unknown schemas, malformed evidence, foreign bytes, drift, unsafe links, or mismatched paths are not adopted.

`recover apply PLAN.json` accepts that unmodified saved JSON output, takes the exclusive mutation lock, and re-reads the evidence. The supplied plan must exactly match a fresh plan for the same store, platform and target before changing the pair. In live mode, read-only native checks require Linux not running or macOS unloaded; an absent target additionally requires safe disabled/no-activation state. Unavailable or ambiguous native state is not permission to proceed. Offline `--root` recovery never invokes the native manager. `recover apply --dry-run` only validates and previews, without acquiring a lock, querying native state, or writing anything.

Application restores only the selected filesystem pair and retains the original journal until that pair has been verified. It does not replace the journal with a new recovery transaction or erase evidence after a partial failure. An interruption during recovery requires a new plan from the resulting state; a completed plan cannot be replayed. Applying it again returns `no_pending_recovery` without changes, while a new read-only plan reports `pending: false`. There is no force option.

Schema-2 targets require an already complete, verified private capture generation. Recovery never initializes, adopts, deletes, or truncates log remnants. An interruption before log initialization completed can instead be rolled back to a safe `before` target, including schema 1 or absence, while retaining the remnants as evidence. A schema-2 `before` target still needs its own complete verified capture.

Native service-manager operations cannot be rolled back transactionally. Recovery never stops, starts, enables, disables, or reloads a service, and it does not restore runtime or persistent native preferences. In particular, it does not run Linux `daemon-reload`. A failed macOS add may have disabled its label before the filesystem failure; filesystem recovery does not undo that preference. Follow the [recovery procedure](TROUBLESHOOTING.md#interrupted-filesystem-change), including private backups and a deliberate native-state review before any later lifecycle action. Published v0.1.0 binaries do not contain the recovery commands.

## Threat and capability limits

No secrets feature; argv, paths, and explicit per-service environment values are ordinary plaintext. Display redaction does not hide them from native managers, child processes, or native inspection tools. No arbitrary shell evaluation, external service adoption, symlink repair, system scope, root, account changes, or lingering changes. Persistent login preference and runtime state are reported separately. Real native timing and GUI-login behavior require isolated host validation; mocks cannot establish those facts.

## Log capture boundary (unreleased)

`logs` implements bounded text sampling/redaction; `process` implements nonblocking, deadline- and byte-limited capture for native helper processes. State parsers refuse incomplete capture and retain only known bounded scalar keys. Ordinary native errors expose fixed hints plus exit/signal evidence, not arbitrary stderr.

macOS capture generations live below a private marked log root. UID, exact private modes, regular-file/nlink checks, no-follow descriptors, exclusive creation, root/generation markers, and derived paths prevent accidental adoption or truncation. New generation initialization follows journal creation and precedes native definition publication. A partial initialization leaves evidence; recovery must not silently delete it. Runtime enable/start/restart validates capture, and restart rechecks after teardown. These checks do not sandbox another process already acting as the same trusted account, nor control launchd's later file opens at login.

Old records are migrated only by explicit stopped update. Definitions and capture are independent of retention: remove retains logs; ID reuse gets a new generation. Manual stopped/unloaded rotation is documented; no daemon or global journal-retention change is introduced.
