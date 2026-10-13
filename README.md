# loboot

A small Rust CLI for native user services: launchd on macOS, systemd --user on Linux. No daemon, database, shell command strings, or root privileges.

**Early v0.1, draft implementation.** Mock/offline verification is separate from real login-session validation. See [verification](docs/IMPLEMENTATION.md). License selection is pending the repository owner's decision; no license grant is implied by this repository being public.

## Build

```sh
cargo build --locked --release
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

Requires Rust 1.99 or newer (the locally tested toolchain). Only macOS and Linux are implementation targets.

## Quick add

```sh
loboot add hello --mode once -- /usr/bin/printf 'Hello from loboot\n'
```

The executable must be absolute; the current directory becomes the working directory unless `--cwd` is given. Use TOML when you want a reusable definition.

## Define a service

```toml
id = "hello"
executable = "/usr/bin/printf"
args = ["Hello from loboot\\n"]
cwd = "/Users/you"
mode = "once"
restart = "never"
```

Use existing, absolute executable and working-directory paths. Arguments are passed directly, without shell expansion. `once` means no automatic retries, not a permanent deduplication guarantee; explicit starts can run it again. `service` supports `never`, `on-failure`, and `always` restart policies. Unknown fields and secret/environment features are rejected. Symlinked input/executable/cwd paths are refused: supply their real canonical paths (for example `/private/tmp` rather than `/tmp` on macOS). Linux requires systemd 240+; executable names containing quotes, backslashes, or glob metacharacters are rejected by the native parser, although ordinary argv can contain them.

```sh
loboot add hello.toml --dry-run --json
loboot add hello.toml
loboot enable hello --dry-run
loboot enable hello
loboot start hello
loboot status hello --json
loboot stop hello
loboot disable hello
loboot remove hello --dry-run
```

`add` installs a disabled definition without starting it. `enable` changes future activation without starting; `start` changes runtime without enabling. On macOS, start/restart of a disabled job is refused: explicitly enable it first. macOS disable requires `stop` first if the job is loaded. `stop` preserves enabled preference. Update and removal require the job to be stopped; removal also requires disabled persistence. Definition updates never restart implicitly.

Linux “login” means user-manager activation. Lingering and overlapping sessions may keep that manager alive across logouts. loboot does not change linger settings. macOS uses the graphical `gui/<uid>` session and `~/Library/LaunchAgents`.

## Safe offline trial

```sh
loboot --root /absolute/path/to/sandbox --platform linux add hello.toml
loboot --root /absolute/path/to/sandbox --platform linux start hello --dry-run --json
loboot --root /absolute/path/to/sandbox --platform linux list
```

`--root` is an explicit offline filesystem sandbox. Native execution is prohibited even if a service manager happens to be available. Cross-platform rendering requires `--root`. Dry-run never creates directories/locks, writes state, calls a manager, or repairs anything. Plans are conditional where current manager state is unknown.

## Safety and limits

- Only exact managed definitions can be changed; external services are read-only.
- Managed paths reject symlinks, traversal, foreign files, and drift. No `--force` adoption.
- Filesystem updates are journaled, not a fictional cross-filesystem transaction. Interrupted writes block further work and preserve before/after evidence. See [recovery](docs/ARCHITECTURE.md#recovery).
- Native command failure reports completed-prefix information; inspect `status` before retrying. A successful command is not proof that a process remains running.
- `discover` lists definition files in the configured native directory only; it is not a machine-wide inventory.
- `logs` currently returns `unsupported_capability`; use native logging tools. Boot/system scope, Windows, secrets, shell interpretation, and automatic adoption are deferred.
- Arguments and paths are stored in plaintext definitions. Do not put passwords or tokens in argv.

[Specification](docs/SPEC.md) · [Architecture](docs/ARCHITECTURE.md) · [CLI](docs/CLI.md) · [Implementation and checks](docs/IMPLEMENTATION.md)
