# Operator guide

loboot manages one account's native services. It does not require a resident loboot process, elevate privileges, or implement its own supervisor. Real logout/login activation acceptance remains pending. Hosted lifecycle checks cover disposable runner accounts; begin with an offline trial and use an isolated account for native evaluation.

## Define a job

Direct argv creation:

```sh
loboot add hello --cwd /absolute/existing/directory --mode once --restart never \
  -- /usr/bin/printf 'Hello from loboot\n'
```

Without `--cwd`, the current physical directory is used. Direct creation defaults to `--mode service --restart never`. For reuse, put all six fields in a TOML file:

```toml
id = "worker"
executable = "/opt/my-app/bin/worker"
args = ["--foreground"]
cwd = "/opt/my-app"
mode = "service"
restart = "on-failure"
```

```sh
loboot explain worker.toml --json
loboot add worker.toml --dry-run --json
loboot add worker.toml
```

Use the paths of your actual application. IDs match `[a-z][a-z0-9-]{0,62}`. All six original TOML fields are required, including `args = []` when there are no arguments. The `[environment]` table described below is optional and unreleased. Unknown fields are errors. Input files, executable/cwd paths, and their ancestors must not be symlinks; executable and cwd must already exist.

The executable runs directly. loboot does not interpret `~`, `$VARIABLE`, globs, pipes, or redirects in stored values. Your invoking shell may still expand unquoted arguments before loboot sees them. Programs can themselves interpret arguments; use only trusted programs. A long-running service should remain in the foreground so the native manager can track it.

### Environment and PATH (unreleased)

Use a manifest, not a direct `--env` flag, to configure a service's non-secret environment. For example, append this table **after the six required fields**:

```toml
[environment]
APP_MODE = "production"
LOG_LEVEL = "info"
PATH = "/opt/tools/bin:/usr/bin:/bin"
```

Values must be quoted TOML strings, including numbers or booleans that your application expects as text. Empty string values are allowed except for `PATH`. Entries are sorted by name. There are limits of 64 entries, 128 ASCII bytes per name, 4096 UTF-8 bytes per value, and 32768 aggregate bytes including names and `=`/NUL overhead. Names match `[A-Za-z_][A-Za-z0-9_]*`; control characters and Unicode noncharacters in values are rejected. See [the exact validation contract](CLI.md#environment-and-path-unreleased).

- Omit `PATH` to leave the native manager's default search path unchanged. The environment of your interactive terminal is not captured/imported, and shell startup files are not sourced.
- Set `PATH` only when the application or its child processes need a specific path. It is a **complete override**, not an append; write every intended absolute directory explicitly. Do not use `$PATH`, `~`, relative directories, empty entries, or `.`/`..` components.
- `PATH` is checked for syntax only. Verify directory existence and trust yourself. It does not select the service executable, which must still be an absolute existing regular executable with no symlink components.
- Values and argv are literal. `$APP_MODE` in an argument does not become the configured value, and no shell commands, virtualenv activation, or runtime/version-manager initialization are performed. Manager defaults may supply other variables; this is not an isolated or fully reproducible environment.

The [Node example](../examples/node-environment.toml) uses an absolute Node executable plus ordinary `NODE_ENV`/application configuration. The [Python example](../examples/python-environment.toml) uses an absolute Python executable and `PYTHONUNBUFFERED`, without activating a virtualenv. Replace all example executable, script, and cwd paths with actual paths for your account. Interpreter/version-manager and virtualenv launchers are often symlinks and may be refused; choosing a runtime installation is your responsibility, and merely setting `VIRTUAL_ENV` does not activate one.

Preview with `loboot explain MANIFEST --json` and `loboot add MANIFEST --dry-run --json`. `list`, `show`, and `explain` display configured names but redact their values. A native definition in that output is marked as a **non-replayable redacted preview**, not an exact backup or a file to install. Retain the original manifest privately; changing environment requires an explicit stopped `update MANIFEST`, never editing the native file or copying its redacted preview.

**Not for secrets:** original values remain plaintext in the manifest, managed record, native definition, and recovery journal. Native tools, the manager, descendants, and application logs may reveal them. The explicit `recover plan --json` export intentionally retains exact raw values for recovery and must stay private; apply/dry-run summaries do not make that input file safe to share. There is no encryption, secret store, or sandbox.

Published v0.1.0 binaries do not support `[environment]`. Omitted/empty environment preserves old renderings and loadable schema-1/schema-2 records in current source; new nonempty records cannot be managed by a pre-feature binary. This compatibility rule is independent of macOS schema-2 capture. Downgrading does not remove environment configuration or restore an earlier record. Keep a compatible binary for services that use it.

### Modes and restart policies

- `once` requires `restart = "never"`. It may run again on an explicit start or later activation; it is not an exactly-once ledger.
- `service` accepts `never`, `on-failure`, and `always`.
- Linux renders `oneshot` or `exec` units with the selected restart policy and a two-second restart delay.
- macOS uses `RunAtLoad`, the corresponding `KeepAlive` setting, and a two-second throttle interval. Native timing and failure classification are platform behavior, not a cross-platform timing guarantee.

## Runtime versus future activation

| Command | Definition | Runtime | Future activation |
| --- | --- | --- | --- |
| `add` | Installs an owned definition | Does not start | Disabled initially |
| `update` | Replaces owned bytes; stop first | Does not start/restart | Not explicitly changed |
| `start` | Unchanged | Requests a start | Not changed |
| `restart` | Unchanged | Requests a restart | Not changed |
| `stop` | Unchanged | Requests a stop/unload | Not changed |
| `enable` | Unchanged | Does not start | Enabled |
| `disable` | Unchanged | Does not stop | Disabled |
| `remove` | Removes owned files | Requires stopped | Requires disabled |

Platform prerequisites matter:

- Linux can explicitly start a disabled service while keeping it disabled.
- macOS requires `enable` before `start` or `restart`. A loaded job must be stopped before `disable`. These refusals prevent hidden temporary preference changes or implicit stops.
- macOS `stop` unloads the job with `bootout`, rather than merely signaling a `KeepAlive` process that could respawn. Stop and loaded restart wait for the old label and observed process IDs to disappear. Restart then rechecks ownership and enabled preference before one bootstrap, avoiding a teardown race or an extra kickstart of a short job. A timeout/uncertain observation stops with partial-progress evidence; inspect status before deciding what to do next.
- Ordinary Linux definition/lifecycle changes include native reloads; explicit filesystem recovery does not. macOS `add` records a disabled native preference before exposing a new plist.

A normal lifecycle is:

```sh
loboot enable worker --dry-run
loboot enable worker
loboot start worker
loboot status worker --json
loboot stop worker
loboot disable worker
```

To update, stop first, edit your source manifest, then run `update` and explicitly start if desired:

```sh
loboot stop worker
loboot update worker.toml --dry-run
loboot update worker.toml
loboot start worker
```

If the macOS job was disabled, enable it before the last command. To remove it, stop, disable, preview removal, then remove. Identical `add` and already-absent owned removal can be no-ops; changing an existing ID requires `update`.

### What “login” means

Linux activation follows `default.target` in the user manager, not every shell or simultaneous login. A lingering user manager or overlapping sessions can survive logout. loboot neither changes lingering nor guarantees one run for each login.

macOS uses the graphical `gui/<uid>` domain and `~/Library/LaunchAgents`. It does not manage the system domain, boot daemons, or another account. A non-graphical session can lack the required domain.

## Inspecting state

```sh
loboot list --json
loboot show worker --json
loboot discover --json
loboot doctor --json
loboot status worker --json
```

- `list`/`show` validate local ownership records and expected native bytes; they do not query the manager. Their public output redacts configured environment values, not the exact stored bytes used for ownership.
- `discover` is a shallow inventory of `.service`/`.plist` files in the configured directory. External entries are read-only. It is not a machine-wide inventory and cannot import services.
- `doctor` reports state health and session availability separately. Its successful outer envelope does not mean every diagnostic passed.
- `status` requires a live session and reports `loaded`, `running`, and `enabled` independently, with `null` where a dimension is unknown.

A native command can succeed even though a short job has already exited or an application later fails. Check the returned observed state and the application's own results. A failure after partial progress does not imply rollback.

### Logs

Source builds after v0.1.0 support bounded output and fixed failure hints:

```sh
loboot status worker --json
loboot logs worker --lines 100 --max-bytes 65536 --json
loboot logs worker --dry-run --json
```

Linux reads the existing user journal without changing its retention or permissions. macOS new definitions capture stdout/stderr under private `~/.local/share/loboot/logs/ID-GENERATION/` directories. `show` reports `log_capture` paths. Existing schema-1 definitions remain unchanged and readable; stop/unload the job and explicitly `update` its original manifest to opt into schema-2 capture, then start only when intended. Identical `add` does not migrate it. Existing captured generations persist across updates. Downgrading the binary to v0.1.0 does not roll back schema-2 records; that older binary cannot manage them.

Reads are bounded per source and report missing, empty, truncation, invalid encoding, and best-effort redaction separately. Never assume arbitrary application-written output is secret-free. Inspect it before sharing, and keep credentials out of arguments and logs. Native exit diagnostics add fixed hints without dumping the environment or arbitrary manager stderr. See the [JSON contract](CLI.md#bounded-logs-and-diagnostics-unreleased).

### Retention and deliberate rotation

Capture files append while launchd runs a job. **There is no unattended disk cap or automatic rotation.** Bounded reads do not bound on-disk growth. Choose programs with appropriate output volume, inspect disk use, and rotate deliberately. Linux journal retention remains the host administrator's responsibility; loboot never vacuums a shared journal.

For macOS rotation, explicitly stop/unload the exact owned job first and verify all of its processes have exited. Use `show ID --json` to verify the exact current generation paths and private ownership (directories 0700, files 0600, no links). Back up only the desired bounded/private history, then truncate the two existing regular files in place using trusted local tools; preserve their ownership, modes and `.owner` markers. Do not move/replace/truncate an active capture or manipulate a shared directory. Inspect with `logs`, then deliberately start again if wanted. Permission repair, rotation and deletion are not implicit loboot actions.

`remove` retains captured data and returns `retained_logs` paths. Reusing an ID allocates a fresh generation and never adopts or truncates old logs. Remove retained files only after verifying their exact former generation, all associated jobs/processes are gone, and any needed evidence is backed up. An unknown/malformed marker or unexpected file means stop and investigate, not recursively delete. An interrupted initialization leaves the normal recovery journal; preserve it and the log remnants for explicit recovery.

## State and native locations

| Data | Location |
| --- | --- |
| Managed record | `$HOME/.local/share/loboot/ID.toml` |
| Linux definition | `$HOME/.config/systemd/user/loboot-ID.service` |
| macOS definition | `$HOME/Library/LaunchAgents/com.loboot.ID.plist` |
| Offline records/definitions | `ROOT/state/` and `ROOT/native/` |
| Mutation lock and recovery journal | State directory's `.lock` and `.pending.json` |

These live paths are based on `HOME`; this version has no XDG or custom live-state-directory override. `HOME` must be absolute and the managed path components must be real directories. `--root` cannot redirect live operations.

A stored record is **not** an input manifest: it is TOML metadata plus a `[manifest]` table and the exact expected native bytes. Keep your original manifest separately. Do not edit generated files or records to change a service; use `update`. Back up the record and definition together, and retain any pending journal before attempting recovery.

## Explicit filesystem recovery (unreleased)

If an interrupted change leaves `.pending.json`, stop normal mutations and preserve private backups. Source builds after v0.1.0 provide `recover plan` and `recover apply`; published v0.1.0 binaries do not. Follow the [full recovery procedure](TROUBLESHOOTING.md#interrupted-filesystem-change) before applying anything.

From a private directory outside the managed state/native directories, save `loboot recover plan --to before --json > recovery.json` to a new file after setting `umask 077`. The default target is `before`; use `--to after` only to deliberately complete the interrupted pair. Review the exact before/after definitions, journal/current-state snapshot, file metadata and intended target. Keep the plan at mode 0600: it is an explicitly sensitive exact export with raw environment values as well as potentially sensitive plaintext arguments and paths. Do not share it or edit it to override checks. Without `--json`, `recover plan` shows only a value-free summary; adding `--dry-run` to a JSON export does not hide the raw values.

`loboot recover apply recovery.json --dry-run --json` previews without writes, locks or native queries, and does not echo raw configured environment values in its summary. `loboot recover apply recovery.json --json` explicitly re-reads under the exclusive lock and refuses a plan that no longer exactly matches. Live apply requires Linux not running or macOS unloaded; an absent target also requires safe disabled/no-activation state. Prepare those conditions deliberately with verified platform tools while ordinary loboot commands are blocked. Recovery never stops, starts, enables, disables or reloads the job, and does not restore native runtime or preferences.

The original journal stays until the selected record/native pair is verified. An interrupted apply needs a fresh plan; a completed replay refuses with `no_pending_recovery` and makes no changes. A fresh plan after completion reports `pending: false`. Unknown, foreign, drifted, symlinked, hardlinked, malformed or stale evidence is not force-repaired.

Both stored record schemas are supported. A schema-2 target needs a complete verified existing capture generation. A partial log initialization can roll back to a safe schema-1/absent `before` target, retaining the remnants for investigation. Recovery never deletes, adopts, initializes or truncates logs; a schema-2 `before` target must also have complete capture. Neither filesystem recovery nor downgrading the binary restores the original native preference.

## Offline and dry-run behavior

`--root /absolute/physical/path` isolates filesystem work and prohibits all native manager execution. `--platform linux|macos` selects rendering only with `--root`. Offline `add`, `update`, `remove`, and explicit `recover apply` can modify sandbox files; runtime commands require `--dry-run` there. Use the same root and platform for recovery planning, preview, and apply. Offline recovery does not query or certify a live job's native state.

Dry-run reads existing files but performs no writes, directory/lock creation, manager calls, or repairs. Native state remains unknown, so a macOS plan can list conditional loaded/unloaded branches. Preconditions are rechecked on apply; a preview is not a transaction or a guarantee that a later invocation will succeed.
