# CLI contract

Global flags: `--json`, `--dry-run`, `--root ABSOLUTE_OFFLINE_DIR`, `--platform linux|macos` (requires `--root`). `--help` and `--version` are informational text even with `--json`.

Commands:

- `add ID [--cwd PATH] [--mode once|service] [--restart never|on-failure|always] -- /absolute/COMMAND ARGS`: direct argv creation; cwd defaults to current canonical directory, mode to service, restart to never.
- `add MANIFEST`: install an owned, disabled definition; repeat identical addition is a no-op.
- `update MANIFEST`: replace an existing owned definition; stop first. Does not restart.
- `remove ID`: remove an owned definition; stop and disable first.
- `list`: managed records; no manager query.
- `show ID`: one exact managed record; drift is an error.
- `discover`: shallow configured-native-directory inventory; foreign entries read-only.
- `status ID`: native observed state, requires a live user session.
- `start|stop|restart ID`: runtime only; no implicit persistence change.
- `enable|disable ID`: future user-manager/login activation only; no implicit start/stop.
- `explain MANIFEST`: validated manifest, generated native definition and capabilities.
- `doctor`: state diagnostics and live-session availability (no session query offline/dry-run).
- `logs ID`: explicit unsupported-capability error in this version.

Every mutating command accepts dry-run. It performs filesystem reads only. Live preconditions are listed but not evaluated; actual execution rechecks them. A plan is not authorization for later external changes and not an atomic promise about future OS state.

## Platform distinctions

Linux disabled services can be started explicitly while remaining disabled. macOS disabled services must first be enabled; loboot refuses start/restart otherwise. Disable a macOS job only after it is unloaded with stop. This narrower matrix avoids hiding launchd's persistent disabled state or interrupting jobs as an unadvertised side effect.

Fresh macOS start bootstraps once; it does not bootstrap and kickstart, which could run a short once job twice. Stop bootouts rather than signals a KeepAlive job that would respawn. Restart rereads the installed definition.

## JSON

Successful output is `{"schema_version":"loboot.cli/v1","ok":true,"data":...}`. Failures are `{"schema_version":"loboot.cli/v1","ok":false,"error":{"code":"...","message":"..."}}`. Native stdout/stderr is captured and never contaminates the JSON stream. Normal failures exit 1; argument parser errors exit 2. `doctor` returns a diagnostic result, so its nested `state.ok` and `session.available` must be checked, not only the envelope.

Codes distinguish usage, validation, unsupported capabilities/platforms, unavailable sessions, foreign collisions, drift, pending recovery, prerequisite errors, native partial failures, and failed final verification. Native observed state includes loaded/running/enabled separately, with null for an unavailable dimension. Consumers must tolerate additional fields and unknown native states. Human messages are not a stable parser interface.
