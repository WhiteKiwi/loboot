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
| `logs ID` | Explicit `unsupported_capability` error after managed-record validation. |

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

Fresh macOS start bootstraps once; it does not bootstrap and kickstart, which could run a short once job twice. Stop bootouts rather than signals a KeepAlive job that would respawn. Restart unloads a loaded job and reloads its installed definition.

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
