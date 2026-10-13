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
| `status_required`, `status_unavailable`, `native_output` | Native observations are incomplete or unrecognized. Retain sanitized raw output; unknown is not stopped/disabled. |
| `unsupported_capability` | The requested feature, currently `logs`, is intentionally unimplemented. Use native/app logging. |

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

`recovery_required` means the state directory contains `.pending.json`. Normal record reads and mutations are blocked; `doctor` reports the blocker. There is no automatic recovery command in this version.

Use a deliberate, manual recovery in the intended account:

1. Stop issuing loboot mutations. Inspect the exact native job with platform tools. If it must be stopped for recovery, stop only that verified owned job; this is a real runtime change.
2. Back up the entire loboot state directory, relevant native definition, and journal. Preserve original bytes and permissions; do not remove evidence first.
3. Read the journal's `schema_version`, `operation`, `id`, `state_dir`, `native_dir`, `before`, and `after`. Verify that its paths and ID are the expected account/resources. Do not execute commands or follow arbitrary paths merely because a damaged journal contains them.
4. Choose one consistent outcome: restore the **before pair**, or complete the **after pair**. A non-null record contains the TOML metadata/`[manifest]` values and exact `native_bytes`. Restore the record as TOML and its native file as those exact bytes. A null record means both corresponding service files should be absent. The JSON journal itself is not a TOML record or input manifest.
5. Verify both files agree with that chosen record and with the current renderer. Preserve correct ownership, restrictive permissions, and non-symlinked paths. If neither candidate is trustworthy, stop and seek review rather than guessing.
6. Only after that verification, remove the pending journal. Run `loboot doctor --json`, then `loboot show ID --json` for a retained record. Inspect native state again before any deliberate reload/start/enable.

The journal covers filesystem pairs, not the service manager's state. Restoring files does not undo an earlier native command or persistent enable/disable preference. macOS add can leave a new label disabled if its subsequent file commit fails. Recovery must preserve that distinction.

Do not delete `.pending.json` just to clear the error, copy one side of a pair alone, blindly replay starts, or use recursive cleanup against a shared native directory.

## Reporting a reproducible issue

Include the source commit, platform/architecture, sanitized command/manifest, exit code, JSON error code, and which stages passed or were never run. Say whether this was `--root`, dry-run, fake-runner testing, or a real user session. Follow [security guidance](SECURITY.md) before posting sensitive logs or exploit details.
