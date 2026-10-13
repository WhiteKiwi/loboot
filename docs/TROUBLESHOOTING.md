# Troubleshooting

Start with `loboot --version`, your exact Git revision, and `loboot doctor --json`. A successful doctor envelope means the diagnostic command ran; inspect `data.state.ok` and, for a live check, `data.session.checked` and `data.session.available`.

Do not retry a mutating command blindly after a native failure. Some steps may already have succeeded. If state reads are blocked, use the recovery procedure below before making further loboot changes.

## Common refusals

| Code or symptom | Meaning and next step |
| --- | --- |
| `session_unavailable` | The user manager/GUI session is absent or unreachable. Run in the intended logged-in account; inspect read-only native status. Do not use sudo or change linger as an automatic workaround. |
| `offline_only` | `--root` intentionally disables live status/runtime actions. Use `--dry-run` for plans, or omit `--root` only when you intend to operate on real account services. |
| `usage` | Check `loboot COMMAND --help`. `--platform` needs `--root`; creation options belong to direct argv `add`, while a TOML file carries its own settings. |
| `invalid_id` | Use lowercase ASCII IDs matching `[a-z][a-z0-9-]{0,62}`. |
| `invalid_manifest`, `invalid_restart` | Supply all six known manifest fields. `once` requires `never`; environment/secret fields are not supported. |
| `invalid_path`, `unsafe_or_inaccessible_path`, `unsafe_or_inaccessible_file` | Check permissions, absolute physical paths, and every symlink ancestor. On macOS use `/private/tmp`, not `/tmp`. Do not replace unrelated symlinks just to make a command pass. |
| `missing_file`, `invalid_executable`, `invalid_cwd` | The input/executable/directory must exist; the executable must be a regular file with executable permissions. |
| `unsupported_executable_path` | Linux's native parser cannot accept quotes, backslashes, or glob metacharacters in the executable pathname. Choose a suitable real executable path. Ordinary arguments have separate escaping. |
| `already_managed` | The ID already has a different managed definition. Stop it and use `update` with its manifest. |
| `not_managed`, `foreign_definition`, `native_collision` | No matching ownership record exists, or an external file/job/activation link uses the name. Choose a fresh ID or investigate the external resource; loboot never adopts it. |
| `definition_drift`, `invalid_state`, `native_conflict` | Record/native bytes, schema, native source path, or drop-ins no longer match. Preserve evidence and reconcile the exact owned files; do not force overwrite or add a forged record. |
| `disabled_service` | On macOS, explicitly enable before start/restart. |
| `stop_first`, `service_not_stopped` | Stop the job before loaded macOS disable or definition update/removal. A finished macOS process may still have a loaded job. |
| `service_not_disabled` | Removal requires a stopped job and disabled future activation. |
| `store_busy`, `stale_plan`, `state_changed` | Another operation is active or the filesystem changed. Wait for it, inspect state, and prepare a fresh plan. Do not delete the lock file to bypass coordination. |
| `no_pending_recovery` | No journal remains for the supplied plan. A completed replay makes no changes; run `recover plan --json` to inspect current pending state rather than recreating the journal. |
| `status_required`, `status_unavailable`, `native_output` | Native observations are incomplete or unrecognized. Retain sanitized raw output; unknown is not stopped/disabled. |
| `unsupported_capability` | The requested backend capability is unavailable; inspect the command and platform. |

## Native partial progress

- `partial_failure` can indicate a native command failed after a completed prefix, an interrupted filesystem commit, or incomplete macOS teardown after bootout. A teardown failure can leave the old job unloaded without starting its replacement; inspect the owned native label and process state before deliberately starting again. Read the message to identify the completed prefix.
- `native_sync_needed` means the filesystem change committed but the subsequent native reload failed. Do not assume files were rolled back. Inspect native state; the next native action retries the reload.
- `verification_failed` means native commands were accepted but the final state query failed. Inspect the exact native job before deciding whether another start/restart is safe.

Read-only native diagnostics for an example ID `worker`:

```sh
# Linux
systemctl --user status loboot-worker.service
systemctl --user show loboot-worker.service
journalctl --user -u loboot-worker.service

# macOS
launchctl print "gui/$(id -u)/com.loboot.worker"
launchctl print-disabled "gui/$(id -u)"
```

A nonzero native status result can mean an inactive/missing job; inspect its content. Do not treat every error as proof of absence. loboot's `status` also requires readable managed state and cannot bypass a pending journal.

## Interrupted filesystem change

`recovery_required` means the state directory contains `.pending.json`. Normal record reads and mutations are blocked; `doctor` reports the blocker. Source builds after v0.1.0 offer explicit `recover plan` / `recover apply`; neither runs automatically. Check `loboot recover --help` on the binary you actually use. Published v0.1.0 binaries do not include these commands.

### Review and recover with a source build

1. Stop issuing normal loboot mutations. Inspect the exact owned native job using platform tools. Live recovery requires Linux not running or macOS unloaded, not merely a process that has finished. When the intended target has no record/definition, future activation must also be safely disabled/absent. If a stop/unload or preference change is needed, perform only that separately authorized change to the verified owned job, then inspect again. Recovery itself does not do it, and ordinary loboot lifecycle commands remain blocked by the journal.
2. Back up the state directory, relevant native definition, journal, and any log remnants privately. Preserve original bytes and permissions. Do not remove evidence, repair permissions, or follow paths merely because a damaged journal mentions them.
3. Choose one consistent filesystem outcome: restore **before** (the default), or complete **after**. A null target means both the service record and native definition should be absent. From a private directory outside the managed state/native trees, save a new plan with restrictive permissions:

   ```sh
   umask 077
   loboot recover plan --to before --json > recovery.json
   ```

   For `after`, request a separate new plan with `--to after`; never edit a saved plan. Check the exit status and `ok` before using its output. A plan without a journal reports `pending: false`; there is nothing to apply. `umask` does not tighten an existing file's permissions, so use a new filename and verify mode 0600. Plans contain exact raw definitions, argv, paths, journal/current-state evidence and file metadata, not sanitized diagnostics. Never share a raw plan.
4. Review the account, root/platform, paths, ID, operation, target, before/after definitions, and current snapshot. Both schemas 1 and 2 are supported, but unknown/malformed data, foreign or drifted bytes, unsafe links, and stale snapshots are refused. If neither outcome can be verified safely, preserve everything and seek review; there is no force option.
5. Preview the unmodified saved plan, then apply only when its target is what you intend:

   ```sh
   loboot recover apply recovery.json --dry-run --json
   ```

   The preview performs no writes, locking, or native queries. It does not verify live stopped/disabled preconditions. Once reviewed:

   ```sh
   loboot recover apply recovery.json --json
   ```

   Apply acquires the exclusive lock, re-reads the evidence, requires an exact fresh plan and checks live native prerequisites before changing the pair. For an offline sandbox, supply the same `--root` and `--platform` to all three commands; no native calls occur and no live safety is certified.
6. On success, run `loboot recover plan --json` and confirm `pending: false`; run `loboot doctor --json`, and `loboot show ID --json` if a record was retained. Inspect native state before any separate, deliberate reload/start/enable. Recovery does not run `daemon-reload` or any stop/start/enable/disable action.

The original journal remains until the selected record/native pair is verified. `recovery_cleanup_uncertain` means that pair was verified but journal removal/durability could not be confirmed; inspect a fresh plan instead of assuming the journal still exists. If apply fails after partial filesystem progress, preserve the original journal and make a **fresh** plan from the new state. Replaying a completed plan returns `no_pending_recovery` without changes, rather than repeating a deletion or write. A missing journal by itself is not permission to reconstruct ownership.

For macOS schema-2 records, the selected target must have a complete, verified existing capture generation. Recovery will not finish an interrupted log initialization. An earlier interruption can safely roll back to a valid schema-1/absent `before` pair while preserving the partial log generation; a schema-2 `before` pair still requires its own complete capture. No recovery direction deletes, adopts, truncates, or initializes log files or remnants.

The journal covers filesystem pairs, not the service manager's state. Restoring files does not undo earlier native commands or persistent enable/disable preferences. macOS add can leave a new label disabled if its subsequent file commit fails. Inspect and address that separately, never by assuming the filesystem target represents native state.

### Published v0.1.0 / unsupported evidence

The published v0.1.0 binary has only a manual recovery procedure. Preserve private backups and obtain careful review before touching the pair: verify the journal's account, paths, schema, operation, ID and before/after records; restore one complete trustworthy pair; verify the stored record, exact native bytes and renderer agree; only then remove the journal and inspect with `doctor` and `show`. A stored record is TOML metadata plus `[manifest]`, not the JSON journal or an ordinary input manifest. v0.1.0 cannot manage schema-2 records.

If the source recovery command refuses unknown/foreign/drifted/malformed evidence, do not treat manual editing as a force-mode workaround. Stop for investigation. Never delete `.pending.json` merely to clear the error, copy only one side of a pair, blindly replay starts, or recursively clean a shared native directory.

## Reporting a reproducible issue

Include the source commit, platform/architecture, sanitized command/manifest, exit code, JSON error code, and which stages passed or were never run. Say whether this was `--root`, dry-run, fake-runner testing, or a real user session. Follow [security guidance](SECURITY.md) before posting sensitive logs or exploit details.

## Log refusals (unreleased)

- `invalid_log_limit`: use 1–10000 lines and 1–1048576 bytes per source.
- `log_capture_unavailable`: legacy macOS definition has no capture. Stop/unload, then explicitly update its original manifest.
- `unsafe_log_path`, `log_ownership`, `log_collision`: preserve evidence; do not adopt existing files, follow links, or silently repair permissions.
- `log_capture_missing`: a capture needed for a future launch is missing. Restore verified private files while the job is stopped; do not start with uncertain paths.
- `logs_unavailable`: a journal query failed, timed out or produced unreliable diagnostics. This is not proof that logs are empty.

`status` supplies bounded fixed failure hints; `logs` may help distinguish executable, cwd and application failures. Redaction is best effort only. Follow [retention and privacy guidance](OPERATOR.md#logs) before rotating or sharing output.
