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
| `show ID` | Read one exact managed record; drift is an error. |
| `discover` | Shallow configured-native-directory inventory; foreign entries remain read-only. |
| `status ID` | Query observed native state; requires a live user session. |
| `start ID`, `stop ID`, `restart ID` | Runtime actions; no implicit preference changes. |
| `enable ID`, `disable ID` | Future user-manager/login activation preference; no implicit start/stop. |
| `explain MANIFEST` | Validate a manifest and show the generated native definition/capabilities. |
| `doctor` | State diagnostics and live-session availability; session query skipped offline/dry-run. |
| `logs ID [--lines N] [--max-bytes N]` | Bounded read-only private macOS capture or Linux user journal; 100 lines / 65536 bytes per source by default. |
| `recover plan [--to before\|after]` | Read-only recovery snapshot; defaults to `before`. Use `--json` to save a plan for apply. |
| `recover apply PLAN.json` | Explicit filesystem-only recovery from the unmodified saved JSON plan; locked revalidation and live stopped/absence checks precede changes. |

Every mutating command accepts dry-run. Live preconditions are listed but not evaluated; actual execution rechecks them. Plans can be conditional where native loaded state is unknown. A preview is not an atomic promise about future OS state.

`--cwd`, `--mode`, and `--restart` customize direct argv creation. A manifest file supplies its own settings; do not use these options to override its fields.

## Manifest validation

All six fields are required: `id`, `executable`, `args`, `cwd`, `mode`, `restart`. See [operator examples](OPERATOR.md#define-a-job).

- ID: `[a-z][a-z0-9-]{0,62}`.
- Paths: absolute UTF-8, no traversal/control characters or symlink components. The input must be a regular UTF-8 TOML file, at most 1 MiB. Relative input filenames are resolved from the current directory; executable/cwd remain absolute.
- Executable: existing regular executable file; cwd: existing directory.
- Arguments: literal strings without control characters. `args = []` is valid. No shell, environment, or secret-field expansion.
- Mode: `once` or `service`. Restart: `never`, `on-failure`, or `always`; `once` requires `never`.
- Unknown fields are rejected. Linux additionally rejects quotes, backslashes, and glob metacharacters in the executable pathname because its native parser cannot accept them; ordinary argv has separate escaping.

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

Save the complete JSON output, including its `loboot.cli/v1` envelope, in a **new private file outside the managed state/native directories**. Choose the intended account, root and platform before planning. For example, from a private directory outside those locations:

```sh
umask 077
loboot recover plan --to before --json > recovery.json
```

Check the exit status and `ok`, then review the chosen target, paths, journal/current-state snapshot, metadata and exact raw before/after native definitions. `before` rolls back the filesystem pair; `after` completes it. The report is not a TOML manifest, and it must not be edited to change the target or override a refusal. Generate a new plan with `--to after` if that is the intended outcome. Plans contain plaintext potentially sensitive argv and paths; `umask 077` protects newly created files, not an existing output file. Keep them private with mode 0600 and do not share raw plans.

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
- `recover apply --dry-run` does not write, lock, or query native state. It cannot satisfy live preconditions for a later apply.
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
