# Real logout/login acceptance

Status: **not run**. [Issue #9](https://github.com/WhiteKiwi/loboot/issues/9) stays open until actual isolated-session evidence is reviewed for both Linux and macOS. This protocol and its synthetic tests are preparation, not evidence that login activation works.

## Authorization and scope

Use an already available, disposable, non-root login account with no existing loboot state. Obtain explicit permission for native changes and **two complete logout/login cycles**. Never use the owner's ordinary working session, create accounts, enable linger, disable security controls, or force other sessions closed as an automatic workaround. If an appropriate account or independent observer is unavailable, record `blocked` and stop the live part.

Keep the same machine, boot, binary, and source revision throughout. Linux activation follows the user manager's lifetime; closing a terminal or opening another SSH session is not sufficient. Linger must already be off and no overlapping login may keep the manager alive. macOS requires the actual graphical `gui/<uid>` domain to disappear and return. Locking the screen and Fast User Switching are not logout.

An authorized observer outside the test account must capture the interval when its manager/GUI domain is absent. Do not keep an observer shell inside the account alive across logout. Record read-only native observations, exact commands, UTC timestamps, and manager/session identities. For Linux, a manager process PID plus start time or a native invocation identity can distinguish lifetimes; for macOS, preserve read-only GUI-domain/session evidence before, while absent, and after. A reused PID, a shell ID, or a changed timestamp by itself is insufficient. If you cannot establish turnover, report `blocked`, not `passed`.

The protocol tests two unique jobs together:

- `once` with `restart = "never"`: one recorded invocation in this observed manager/session activation.
- `service` with `restart = "always"`: one invocation that remains running until explicitly stopped.

This does not establish globally exactly-once execution or every-simultaneous-login semantics.

## Safe preparation

Build and test the exact revision first. Use Python 3.11+ and absolute physical paths with no symlink components (for example `/private/tmp`, not `/tmp`, on macOS). Resolve the selected Python interpreter and loboot binary to their actual files. Keep the fixture directory below the disposable account's real home, mode 0700; keep its two event files mode 0600. Do not put secrets in the test or evidence.

These tools are offline only:

```sh
python3 scripts/login_acceptance.py template --system Linux > /absolute/evidence/report.json
# On macOS use --system Darwin.
python3 scripts/login_acceptance.py inventory /absolute/native/directory > /absolute/evidence/native-before.json
python3 scripts/login_acceptance.py inventory /absolute/loboot/state > /absolute/evidence/state-before.json
```

Use a new private evidence directory; redirection must not overwrite earlier evidence. The generated report starts with `status: not_run`, false authorization fields, and a random 32-hex `run_id`. Copy its exact two fixture IDs. Fill authorization fields only from actual approval, with a reference understandable to the reviewer. A JSON boolean is an attestation, not permission by itself.

Record OS version, architecture, UID/home, unchanged boot identity, Linux linger state, full source SHA, binary SHA-256, and `loboot --version`. Set `fixture_directory` and `python` to the actual absolute physical paths. Copy `scripts/login_fixture.py` to `fixture_directory/fixture.py`; retain a hashed copy as `fixture_source` evidence. Create **empty** `once.jsonl` and `service.jsonl` files there. The recorder refuses missing, public, hardlinked, or symlinked log files and never contacts a native manager.

The native directories are `~/.config/systemd/user` on Linux and `~/Library/LaunchAgents` on macOS. State is `~/.local/share/loboot`. Inventory these exact paths before installation; the helper preserves symlink targets without following them and refuses special files, oversized inputs, and unsafe paths. Limits are 4 MiB per regular file, 64 MiB total file contents, 10,000 entries and 32 directory levels. An inventory refusal is a blocker, not a reason to omit unrelated entries.

Create the two manifests using their unique IDs and the following field values; substitute the actual paths, ID, run ID and mode:

```toml
id = "loboot-login-<32-hex-run-id>-once"
executable = "/physical/path/to/python3"
args = ["/home/disposable/fixture/fixture.py", "--run-id", "<32-hex-run-id>", "--mode", "once", "--log", "/home/disposable/fixture/once.jsonl"]
cwd = "/home/disposable/fixture"
mode = "once"
restart = "never"
```

The service manifest uses its `-service` ID, `--mode service`, `service.jsonl`, `mode = "service"`, and `restart = "always"`. Do not directly execute the fixture as a preflight: that would add a run and invalidate the zero-run baseline. Unit tests cover recorder mechanics without native activation.

## Five observation stages and two real boundaries

All commands below are deliberate live operations in the approved test account. Keep a command transcript. Do not run `start`, `restart`, `bootstrap`, `kickstart`, or equivalent manual activation during either login cycle.

1. **Install and arm.** Inspect each `add MANIFEST --dry-run`, then `add MANIFEST`. Verify disabled preference and zero event lines. `enable ID` for each job, verifying it did not start either one. Save `show ID --json` for both fixtures. Record stage `armed`: its timestamp, each `status ID --json`, and a copy of each still-empty event log. Both must be enabled and not running; macOS jobs must be unloaded.
2. **Enabled boundary.** The user performs the approved logout. The external observer records a real absent manager/GUI domain, then the user logs in again. Preserve unchanged boot identity, distinct before/after session/manager identities, logout/absence/login timestamps, and native proof. Do not infer absence from a generic permission/query error. Record the login-start timestamp from session evidence, not when the shell was opened later.
3. **Activated.** After the once fixture has exited, record stage `activated` with statuses and copied event logs. Each log must contain exactly one matching invocation after login began. Both jobs must remain enabled; once must not be running, service must be running. macOS jobs remain loaded. Unexpected duplicates or restarts are failures to investigate, not lines to delete.
4. **Stop and disable.** Explicitly `stop ID` and `disable ID` for both jobs. On macOS, stop must unload before disable. Verify no additional event appeared. Record stage `disabled` with both disabled and not running, and macOS unloaded. Preserve the same manager/session identity reached in the first cycle.
5. **Disabled boundary.** Perform the second separately approved logout/login. Capture another observed absence and a third distinct manager/session identity on the same boot. Record stage `returned` immediately after the session is available, then stage `observed` **at least 30 seconds later**. Both jobs must remain disabled/not running and their event logs byte-for-event identical to `activated`; macOS must remain unloaded. This is bounded nonactivation evidence for the observation window, not a guarantee about every future event.

Each stage stores `{at, jobs: {once: {status, events}, service: {status, events}}}` in the template. Each artifact reference is `{"path":"relative-file-name","sha256":"64-lowercase-hex-digest"}` relative to the report directory. Store separate immutable snapshots; never point every stage at a changing live log. Obtain hashes from the actual captured bytes. Preserve the original raw observations privately; a human must verify that the session proof supports the claimed identities/times.

A boundary additionally records `real_logout_login`, `no_manual_activation`, `session_absent_observed`, the authorized observer, exact identities/times, unchanged boot identity, and a `proof` artifact. Both statuses and original native evidence matter; a successful command exit alone does not establish process outcome.

## Cleanup and review

Cleanup is explicit even after failure:

1. Inspect both exact owned IDs, stop any remaining fixture, disable it, and remove it with loboot. Never touch a foreign ID or reset the complete launchd disabled-services domain.
2. Confirm both native jobs are absent and all recorded fixture PIDs have exited; preserve scoped native evidence. If cleanup fails, retain errors and do not mark the run passed.
3. Restore only test-created filesystem resources. If state/native directories did not exist before, remove the test-created lock only after all loboot commands have exited, then use nonrecursive empty-directory removal. If a directory already existed, do not delete it or unrelated entries. If unexpected content remains, stop and preserve it. Keep evidence outside directories being removed.
4. Capture final native/state inventories with the same helper. They must exactly match the initial inventories, including whether each root existed. macOS's two unique disabled-label overrides may remain until disposal of the isolated account; list exactly those labels in `retained_disabled_labels`, and never erase the whole domain to hide them.
5. Record cleanup timestamp, exact removed IDs, `only_test_owned`, `jobs_absent`, `processes_exited`, empty errors, inventory references and proof. Delete the private fixture folder only after its evidence is copied and all fixture processes have exited.

Set `status` to `passed` only after actual observations and cleanup satisfy every condition. Otherwise use `not_run`, `blocked`, or `failed` with a reason. Validate the report:

```sh
python3 scripts/login_acceptance.py validate /absolute/evidence/report.json
```

Exit 0 means the **operator report is internally consistent**, 77 means it reports a non-pass, and 1 means malformed/inconsistent/unreadable evidence. Every result says `independently_verified: false`. Hashes detect artifact changes; they do not authenticate a session transition. A reviewer must inspect original native/session proof, fixture source, command transcript, and cleanup before recording real platform acceptance. Never upload unreviewed session data, usernames, private paths, or unrelated native definitions in a public issue.

No real result is included in this change. The synthetic report builders in `scripts/test_login_acceptance.py` are test data, never acceptance artifacts. Keep [issue #9](https://github.com/WhiteKiwi/loboot/issues/9) open for each unverified platform and link reviewed, sanitized evidence only after the corresponding live protocol has actually run.
