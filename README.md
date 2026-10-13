# loboot

[![CI](https://github.com/WhiteKiwi/loboot/actions/workflows/ci.yml/badge.svg)](https://github.com/WhiteKiwi/loboot/actions/workflows/ci.yml)

Native user services, explicit plans, no daemon.

`loboot` installs and manages your own launchd jobs on macOS and systemd user services on Linux. Runtime actions and login activation stay separate. There is no background loboot process, database, or implicit shell evaluation.

**Early v0.1.** Source builds only. Linux/macOS CI checks use fake service managers and offline filesystems; real login-session lifecycle validation is still pending. The license is undecided, and public source availability does not grant a license. See [verification](docs/IMPLEMENTATION.md) and [release gates](docs/RELEASE.md).

## Build and install

Requires Rust **1.99.0**, Git, and a native compiler/linker on macOS or Linux. The repository pins the development toolchain. Live Linux operations require systemd 240+ and a reachable user manager; macOS operations require a graphical login session.

```sh
git clone https://github.com/WhiteKiwi/loboot.git
cd loboot
cargo install --path . --locked
loboot --version
```

Ensure Cargo's bin directory is on your `PATH`. There is no published crates.io package, Homebrew formula, download installer, or supported Windows build. Installing the binary does not register or start a service. [Installation, updating, and uninstalling →](docs/INSTALL.md)

## Try the workflow safely

This writes only an explicit sandbox and never contacts a service manager. Resolving the temporary directory with `pwd -P` avoids symlinked paths such as macOS `/tmp`.

```sh
sandbox="$(mktemp -d)"
sandbox="$(cd "$sandbox" && pwd -P)"

loboot --root "$sandbox" --platform linux add hello --mode once \
  -- /usr/bin/printf 'Hello from loboot\n'
loboot --root "$sandbox" --platform linux start hello --dry-run --json
loboot --root "$sandbox" --platform linux list --json
loboot --root "$sandbox" --platform linux remove hello
```

Use `--platform macos` in a separate sandbox to inspect launchd plans. `--root` always means offline: an actual `start`, `status`, or other runtime operation is refused. `--dry-run` performs filesystem reads only and does not create directories, locks, definitions, or recovery changes.

## Manage a real user service

The following commands change your login account's service configuration. Read the [operator guide](docs/OPERATOR.md) first and use a unique ID. The executable must be an existing absolute, non-symlinked path; arguments are passed literally. The current physical directory becomes the working directory unless `--cwd` is supplied.

```sh
loboot doctor --json
loboot add hello --mode once --dry-run -- /usr/bin/printf 'Hello from loboot\n'
loboot add hello --mode once -- /usr/bin/printf 'Hello from loboot\n'
loboot enable hello
loboot start hello
loboot status hello --json

# Stop runtime activity before disabling future activation and removing the definition.
loboot stop hello
loboot disable hello
loboot remove hello --dry-run
loboot remove hello
```

`add` installs a disabled definition without starting it. `enable` permits future activation without starting it now; `stop` leaves the enabled preference unchanged. On macOS, enable before start/restart and stop before disabling a loaded job. Updates require a stopped job and never restart it implicitly.

`once` means no automatic retries, not permanent deduplication. Explicit starts and later user-manager/login activations can run the command again. Linux activation follows the user manager's lifetime, which lingering or overlapping sessions may extend beyond a logout. loboot never changes linger settings.

## Reusable definitions

Save a TOML manifest with real paths for your account:

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

Modes are `once` and `service`. Restart policies are `never`, `on-failure`, and `always`; `once` requires `never`. Unknown fields, environment/secret configuration, relative executable/cwd paths, and symlinked paths are rejected. See the [CLI contract](docs/CLI.md) for validation and JSON details.

## Capability matrix

| Capability | Linux | macOS |
| --- | --- | --- |
| Native backend | systemd `--user` | launchd `gui/<uid>` |
| Add/update/remove owned definitions | Implemented | Implemented |
| Start/stop/restart and observed status | Implemented | Implemented; enable before start/restart |
| Separate future activation preference | Implemented | Implemented; stop before loaded-disable |
| Offline plans, dry-run, versioned JSON | Implemented | Implemented |
| Read-only discovery | Configured native directory only | Configured native directory only |
| Real login-session acceptance | Pending | Pending |
| Built-in logs, secrets, boot/system scope | Deferred | Deferred |

Windows, adoption of external services, automatic recovery, and release/package distribution are also outside the current implementation.

## Safety and automation

- Only exact managed definitions may be changed. Foreign files, native overrides, drift, unsafe paths, and interrupted filesystem transactions cause explicit errors; there is no force-adoption flag.
- `--json` returns a `loboot.cli/v1` envelope. Check both the exit status and `ok`; `doctor` also requires checking its nested state/session diagnostics.
- Native failures can occur after some commands succeeded. Inspect status before retrying; command acceptance is not proof that a process remains running.
- Arguments and paths are plaintext. Do not put credentials in them. loboot does not sandbox the program you choose to run.
- Cross-directory filesystem updates are journaled; manager actions are not transactional. Follow the [recovery procedure](docs/TROUBLESHOOTING.md#interrupted-filesystem-change) instead of deleting a journal blindly.

## Documentation

[Docs index](docs/README.md) · [Operator guide](docs/OPERATOR.md) · [Troubleshooting](docs/TROUBLESHOOTING.md) · [Security](docs/SECURITY.md) · [Contributing](CONTRIBUTING.md) · [Changelog](CHANGELOG.md)

[Specification](docs/SPEC.md) · [Architecture](docs/ARCHITECTURE.md) · [CLI contract](docs/CLI.md) · [Verification](docs/IMPLEMENTATION.md) · [Release process](docs/RELEASE.md)
