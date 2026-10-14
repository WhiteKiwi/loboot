# CLI contract

Run `loboot --help` or `loboot COMMAND --help` for parser syntax. Global flags may appear before the command or after its options, but put loboot flags before the direct-execution `--` separator.

## Global options

- `--json`: versioned machine output.
- `--dry-run`: filesystem reads/planning only; no writes or manager queries.
- `--root ABSOLUTE_OFFLINE_DIR`: isolated `state/` and `native/` directories; native manager execution is prohibited.
- `--platform linux|macos`: cross-platform rendering, requires `--root`.
- `--help`, `--version`: informational text, even when combined with `--json`.

## Commands

| Command | Contract |
| --- | --- |
| `add ID [--cwd PATH] [--mode once\|service] [--restart never\|on-failure\|always] -- /absolute/COMMAND ARGS` | Direct argv creation. Defaults: current physical cwd, `service`, `never`. |
| `add MANIFEST` | Install an owned disabled definition; an identical addition is a no-op. |
| `update MANIFEST` | Replace an existing owned definition; stop first. Does not restart. |
| `remove ID` | Remove an owned definition; stop and disable first. |
| `list` | Read managed records; no manager query. |
| `show ID` | Validate one exact managed record and display its public projection; configured environment values are redacted and drift is an error. |
| `discover` | Shallow configured-native-directory inventory; foreign entries remain read-only. |
| `status ID` | Query observed native state; requires a live user session. |
| `start ID`, `stop ID`, `restart ID` | Runtime actions; no implicit preference changes. |
| `enable ID`, `disable ID` | Future user-manager/login activation preference; no implicit start/stop. |
| `explain MANIFEST` | Validate a manifest and show capabilities plus a native-definition preview; configured environment values are redacted. |
| `doctor` | State diagnostics and live-session availability; session query skipped offline/dry-run. |
| `logs ID [--lines N] [--max-bytes N]` | Bounded read-only private macOS capture or Linux user journal; 100 lines / 65536 bytes per source by default. |
| `recover plan [--to before\|after]` | Read-only recovery summary; defaults to `before`. `--json` explicitly exports sensitive exact evidence for apply, including with `--dry-run`. |
| `recover apply PLAN.json` | Explicit filesystem-only recovery from the unmodified saved JSON plan; locked revalidation and live stopped/absence checks precede changes. |

Every mutating command accepts dry-run. Live preconditions are listed but not evaluated; actual execution rechecks them. Plans can be conditional where native loaded state is unknown. A preview is not an atomic promise about future OS state.

`--cwd`, `--mode`, and `--restart` customize direct argv creation. A manifest file supplies its own settings; do not use these options to override its fields.

## Manifest validation

The six original fields remain required: `id`, `executable`, `args`, `cwd`, `mode`, `restart`. Source builds with environment support additionally accept optional `[environment]`; no other required field becomes optional. See [operator examples](OPERATOR.md#define-a-job).

- ID: `[a-z][a-z0-9-]{0,62}`.
- Paths: absolute UTF-8, no traversal/control characters or symlink components. The input must be a regular UTF-8 TOML file, at most 1 MiB. Relative input filenames are resolved from the current directory; executable/cwd remain absolute.
- Executable: existing regular executable file; cwd: existing directory.
- Arguments: literal strings without control characters. `args = []` is valid. No shell evaluation, variable/tilde expansion, or secret-field expansion, including when environment entries are configured.
- Mode: `once` or `service`. Restart: `never`, `on-failure`, or `always`; `once` requires `never`.
- Unknown fields are rejected. Linux additionally rejects quotes, backslashes, and glob metacharacters in the executable pathname because its native parser cannot accept them; ordinary argv has separate escaping.

### Environment and PATH (unreleased)

`[environment]` is an optional map of case-sensitive names to **string** values, rendered in sorted name order. Omitted and empty maps are equivalent. There is no `--env` direct-creation option; use a manifest with `add`, `update`, or `explain`.

- At most 64 entries.
- Names: 1–128 ASCII bytes, matching `[A-Za-z_][A-Za-z0-9_]*`.
- Values: 0–4096 UTF-8 bytes each; control characters and Unicode noncharacters are rejected. Numbers, booleans, arrays, and tables are not string values.
- Aggregate bound: 32768 bytes, computed as the sum of `name.len() + value.len() + 2` for every entry. The two bytes account for `=` and the terminating NUL; lengths are byte counts, not character counts.
- `PATH`, when present, must be nonempty and contain only colon-separated absolute directories, with no empty entry, relative entry, `.` or `..` component. Validation is syntax-only: it does not prove those directories exist, are safe/trusted, or contain a command.

An omitted `PATH` leaves native-manager defaults unchanged. An explicit `PATH` is a complete literal override, not an append to an inherited value: `PATH = "/opt/tools/bin:/usr/bin:/bin"` is valid syntax; `PATH = "$PATH:/opt/tools/bin"`, `PATH = ":/usr/bin"`, and `PATH = "/usr/bin:"` are not. loboot does not capture/import the invoking shell's environment, run an activation script, or change the manager's global environment. Manager/platform defaults can still supply other variables; this is not a clean-room environment.

The executable remains an absolute, verified file path and is not resolved through the configured `PATH`. Values and argv remain literal: no variable references, tilde, command substitution, shell startup files, or virtualenv activation are evaluated by loboot. A launched program can interpret its own environment; trust that program and the directories you configure.

`list`, `show`, and `explain` expose public projections rather than serializing raw stored environment values. Configured names remain visible; each value is replaced by `[REDACTED]`, including within the displayed native-definition preview. The JSON `data.environment_redacted` boolean indicates whether any configured environment was redacted (across all records for `list`); it is false for an omitted/empty map. A redacted definition is clearly marked **non-replayable** and must not be installed, copied over managed files, or used as an exact backup. Ordinary plan/apply/dry-run summaries and environment-validation errors do not echo configured values. Invalid names, limits, value characters, or `PATH` syntax use `invalid_environment`; invalid TOML and non-string values use `invalid_manifest`. This does not sanitize unrelated argv/paths or arbitrary application log output.

The original manifest, owned record, native definition, and journal retain exact plaintext values. The explicit `recover plan --json` export is a sensitive exception: its raw values are necessary for exact-match recovery, and editing/redacting that saved plan makes it unsuitable for apply. See [recovery privacy](#explicit-recovery-unreleased) and [security](SECURITY.md#plaintext-data-and-reports).

Existing schema-1 and schema-2 records with omitted/empty environment keep their previous exact rendering. New nonempty environment records require a binary with this feature; older binaries reject them. Environment configuration does not itself introduce a record schema or opt into macOS log capture: schema 2 remains the separate log-capture format. No published v0.1.0 bytes or release guarantees change.

## Platform distinctions

Linux disabled services can be started explicitly while remaining disabled. macOS disabled services must first be enabled; start/restart otherwise returns `disabled_service`. Disable a macOS job only after it is unloaded with stop.

Fresh macOS start bootstraps once; it does not bootstrap and kickstart, which could run a short once job twice. Stop bootouts rather than signals a KeepAlive job that would respawn. Stop and loaded restart use bounded read-only polling to wait for label removal and exit of observed old PIDs. Restart rechecks ownership and enabled preference before reloading its definition with a single bootstrap. The wait allows 100 observations with 99 pauses of 100 ms, plus native-query time; it is not a hard ten-second deadline. Exhausted observations or uncertainty report partial progress and never automatically retry bootstrap/kickstart.

Linux activation follows the user manager's lifetime, not each shell login. macOS uses the current `gui/<uid>` session. Neither backend manages boot/system scope.

## JSON and exit behavior

Success:

```json
{"schema_version":"loboot.cli/v1","ok":true,"data":{}}
```

Failure:

```json
{"schema_version":"loboot.cli/v1","ok":false,"error":{"code":"offline_only","message":"..."}}
```

These show the envelope shape, not the full data for a particular command. Native stdout/stderr is captured rather than interleaved with JSON. Runtime failures exit 1; argument-parser errors exit 2; successful commands exit 0. Help/version remain text and are not JSON records.

`doctor` returns a successful diagnostic envelope even when a nested check fails. For a real-session readiness check with optional `jq` installed:

```sh
if result="$(loboot doctor --json)"; then
  printf '%s\n' "$result" | jq -e '
    .schema_version == "loboot.cli/v1" and .ok == true and
    .data.state.ok == true and .data.session.checked == true and
    .data.session.available == true'
else
  printf '%s\n' "$result" >&2
  exit 1
fi
```

Offline/dry-run doctor deliberately reports `session.checked = false`; it cannot certify a live session. To check only local state, test `.data.state.ok` without requiring live-session availability.

Automation should:

1. Check the command's exit status and envelope `ok`.
2. Branch on `error.code`, not human wording. Preserve an unrecognized code for diagnosis.
3. Treat `loaded`, `running`, and `enabled` independently; `null` means unknown, not false.
4. Tolerate additional fields and unknown native state strings.
5. Inspect observed state after partial progress rather than blindly retrying starts or restarts.

Common codes and recovery steps are in [troubleshooting](TROUBLESHOOTING.md). The default pretty-printed human output and error messages are not a stable parser interface.

## Explicit recovery (unreleased)

`recover` resolves an interrupted filesystem transaction only when its journal and current files still describe a provably owned before/after pair. It does not repair arbitrary drift or adopt external services. The immutable v0.1.0 release has no `recover` command.

Without `--json`, `recover plan` displays only a value-free summary of the ID, target and changes, not a replayable saved plan. **`recover plan --json` is an explicit sensitive export**, not a bug-report diagnostic: it retains exact raw configured environment values in the evidence. This remains true with `--dry-run`, which changes side-effect behavior, not export sensitivity. By contrast, `recover apply --dry-run` displays a value-free summary of the supplied private plan.

Save the complete JSON output, including its `loboot.cli/v1` envelope, in a **new private file outside the managed state/native directories**. Choose the intended account, root and platform before planning. For example, from a private directory outside those locations:

```sh
umask 077
loboot recover plan --to before --json > recovery.json
```

Check the exit status and `ok`, then review the chosen target, paths, journal/current-state snapshot, metadata and exact raw before/after native definitions. `before` rolls back the filesystem pair; `after` completes it. The report is not a TOML manifest, and it must not be edited to change the target or override a refusal. Generate a new plan with `--to after` if that is the intended outcome. Plans are explicitly sensitive plaintext exports containing exact configured environment values as well as potentially sensitive argv and paths; `umask 077` protects newly created files, not an existing output file. Keep them private with mode 0600 and do not share raw plans.

Preview the saved plan without effects:

```sh
loboot recover apply recovery.json --dry-run --json
```

Only after reviewing the plan and satisfying the [recovery prerequisites](TROUBLESHOOTING.md#interrupted-filesystem-change), explicitly apply it:

```sh
loboot recover apply recovery.json --json
```

- Planning is always read-only: no filesystem writes, lock creation/acquisition, or native queries. With no journal, a fresh plan reports `pending: false`.
- Apply re-reads under the exclusive lock and requires an exact fresh-plan match. Changed bytes or metadata, a different root/platform, or malformed/unknown evidence refuse recovery. Parsing is bounded to 64 MiB and rejects unknown fields; JSON whitespace/key order are not significant. There is no force flag or plan editing escape hatch.
- Live apply uses read-only native checks: Linux must be inactive/failed with no reported reload requirement; macOS must be unloaded. If the selected target is absence, future activation must also be verifiably disabled. Linux additionally queries current enablement and refuses activation links in the configured native directory. Unknown state is not a pass. Recovery performs no native mutation, including `daemon-reload`. A reported or uncertain reload requirement blocks recovery: inspect native state and deliberately reload outside recovery before generating a fresh plan. These queries are observations, not an atomic guarantee against changes by the account owner.
- `--root` permits only offline filesystem recovery. Pass the same root and platform when planning, previewing, and applying; it never contacts a manager or certifies a live service safe.
- `recover apply --dry-run` does not write, lock, or query native state. It cannot satisfy live preconditions for a later apply. Apply and dry-run summaries do not echo raw configured environment values; this does not make the input plan safe to share.
- Stored record schemas 1 and 2 are supported. A schema-2 target requires its complete verified existing log capture. Incomplete initialization can safely roll back to a valid schema-1/absent `before` target while retaining log remnants. Recovery never creates, adopts, deletes, or truncates capture files.
- The original journal remains until the selected record/native pair is verified. After any partial application, inspect the evidence and generate a fresh plan, rather than replaying the old one. After successful completion, replay returns `no_pending_recovery` with no changes; a new plan reports `pending: false`.

The journal and plan cover filesystem definitions only. Native runtime and enable/disable preferences are not restored. Inspect native state before a separate, deliberate reload or lifecycle action.

## Bounded logs and diagnostics (unreleased)

`logs` validates the owned record first. Limits are 1–10000 lines and 1–1048576 bytes **per source**. macOS returns two `sources` in stdout/stderr order, tailing each file oldest-first. Linux returns one combined source using `journalctl --user --unit=... --no-pager --quiet --output=cat --reverse`; journal entries are newest-first, while multiline text within an entry keeps its order. No follow/tail daemon is started.

Each source contains `source`, `stream`, `path` (null for journal), `order`, `text`, `missing`, `empty`, `truncated`, `bytes_truncated`, `lines_truncated`, `invalid_utf8`, `redacted`, `bytes_returned`, and `lines_returned`. Limits apply again after lossy UTF-8 decoding and best-effort common-token redaction. Truncation can split a line or character at the sampling boundary; invalid bytes are replaced and reported. An empty journal result does not prove the application never wrote output: permissions, retention, and native configuration can limit availability. Missing macOS capture files are reported separately from empty files. Unsafe paths fail, never masquerade as empty logs.

`--root --platform macos logs` reads only sandbox files; Linux `--root logs` returns `offline_only`. Any `logs --dry-run` reports its planned source and bounds without querying a manager, reading log content, writing files, or locking. Legacy macOS schema-1 records return `log_capture_unavailable`; `stop`, then explicitly `update` the original manifest to enable capture. Unknown, zero, negative, or excessive limits are errors.

`status` adds fixed `diagnostics` hints to unchanged observed native fields. Exit codes/signals may describe a previous run; neither hints nor command acceptance prove current process success. Native status retains bounded allowlisted scalar fields, not environment dumps or arbitrary stderr.

All native helper invocations have a 1 MiB stdout / 64 KiB stderr capture budget and a 10-second observation deadline, plus up to 100 ms cleanup and scheduling overhead. Journal stdout uses the requested byte bound plus a probe byte. Over-limit or timed-out helpers are terminated in their own process group; managed service processes are not targeted. Incomplete state/ownership output is rejected. A mutation may already have reached the native manager before its helper fails; inspect state before retrying. These budgets are not a hard real-time guarantee.

See [operator retention and privacy guidance](OPERATOR.md#logs). These additions are not present in the immutable v0.1.0 release.
