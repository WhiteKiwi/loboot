<p align="center">
  <img src="docs/assets/loboot-hero.png" alt="Loboot: your apps, ready at login. Roki launches app windows from a laptop, with macOS and Linux shown." width="800">
</p>

<h1 align="center">loboot</h1>

<p align="center">
  <strong>Your commands. Your OS. Ready at login.</strong><br>
  Native user services for macOS and Linux, without another daemon.
</p>

<p align="center">
  <a href="https://github.com/WhiteKiwi/loboot/actions/workflows/ci.yml"><img src="https://github.com/WhiteKiwi/loboot/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> ·
  <a href="#why-loboot">Why loboot</a> ·
  <a href="#native-by-design">Platforms</a> ·
  <a href="docs/README.md">Documentation</a>
</p>

`loboot` turns a command or TOML file into an owned **launchd job** or **systemd user service**. Preview the plan, enable future activation, and control the running job explicitly. No background loboot process, database, or implicit shell evaluation.

> **v0.1.0 · Experimental.** [Native archives are available](https://github.com/WhiteKiwi/loboot/releases/tag/v0.1.0). Isolated hosted lifecycle checks passed on macOS/Linux x64 and ARM64; real logout/login acceptance is still pending. Ordinary CI uses fake service managers and offline filesystems. MIT OR Apache-2.0 licensed. [Verification](docs/IMPLEMENTATION.md) · [Release process](docs/RELEASE.md)

## Quickstart

### 1. Install

Install through [WhiteKiwi’s Homebrew tap](https://github.com/WhiteKiwi/homebrew-tap):

```sh
brew install WhiteKiwi/tap/loboot
loboot --version
```

Alternatively, download the matching macOS/Linux x64 or ARM64 archive from [GitHub Releases](https://github.com/WhiteKiwi/loboot/releases) and verify its SHA-256 checksum. macOS binaries are unsigned/unnotarized; Linux binaries require glibc 2.39 or newer. [Detailed installation](docs/INSTALL.md).

To build from source:

Requires **Rust 1.99.0**, Git, and a native compiler/linker on macOS or Linux.

```sh
git clone https://github.com/WhiteKiwi/loboot.git
cd loboot
cargo install --path . --locked
loboot --version
```

Keep Cargo's bin directory on your `PATH`. Installing the binary never registers or starts services. There is no crates.io package or shell installer. [Install, update, and uninstall →](docs/INSTALL.md)

### 2. Preview a service safely

Try the workflow in an offline sandbox. These commands never contact a service manager:

```sh
sandbox="$(mktemp -d)"
sandbox="$(cd "$sandbox" && pwd -P)"

loboot --root "$sandbox" --platform linux add hello --mode once \
  -- /usr/bin/printf 'Hello from loboot\n'
loboot --root "$sandbox" --platform linux start hello --dry-run --json
loboot --root "$sandbox" --platform linux list --json
loboot --root "$sandbox" --platform linux remove hello
```

Use `--platform macos` in a separate sandbox for launchd plans. `--root` always means offline; live runtime operations are refused. `--dry-run` reads only, without creating directories, locks, definitions, or recovery changes. Resolving the sandbox with `pwd -P` avoids symlinked paths such as macOS `/tmp`.

### 3. Make it a real user service

These commands change your login account's service configuration. Read the [operator guide](docs/OPERATOR.md) first, choose a unique ID, and use an existing absolute, non-symlinked executable path. Live Linux operations need systemd 240+ and a reachable user manager; macOS needs a graphical login session.

```sh
loboot doctor --json
loboot add hello --mode once --dry-run -- /usr/bin/printf 'Hello from loboot\n'
loboot add hello --mode once -- /usr/bin/printf 'Hello from loboot\n'
loboot enable hello
loboot start hello
loboot status hello --json

# Clean up explicitly, in order.
loboot stop hello
loboot disable hello
loboot remove hello --dry-run
loboot remove hello
```

**Add → enable → start.** `add` installs a disabled definition; `enable` allows future activation without starting it now. `stop` leaves that preference unchanged. On macOS, enable before start/restart and stop before disabling a loaded job. Updates require a stopped job and never restart it implicitly.

`once` means no automatic retries, not permanent deduplication. Explicit starts and later user-manager/login activations can run it again. Linux activation follows the user manager's lifetime, which may outlast a logout; loboot never changes linger settings.

## Why loboot

- **Native by default.** Your OS runs the job. loboot plans and manages its definition.
- **Separate intent from runtime.** Future activation and starting or stopping now are distinct actions.
- **Inspect before changing.** Offline roots, read-only dry runs, and `explain` make plans reviewable.
- **Script with a contract.** Versioned `loboot.cli/v1` JSON for automation; literal argv for execution.
- **Own only what you created.** Foreign definitions, overrides, drift, unsafe paths, and interrupted writes cause explicit errors, not force-adoption.

## A command today. A manifest tomorrow.

For a reusable service, save a TOML file with real paths for your account:

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

Modes: `once` or `service`. Restart policies: `never`, `on-failure`, or `always`; `once` requires `never`. Arguments are literal, and the current physical directory is the default working directory for command-based definitions. Unknown fields, environment/secret configuration, relative executable/cwd paths, and symlinked paths are rejected. [Full CLI contract →](docs/CLI.md)

## Native by design

| | Linux | macOS |
| --- | --- | --- |
| Native backend | systemd `--user` | launchd `gui/<uid>` |
| Owned definitions and lifecycle | Implemented | Implemented |
| Future activation preference | Separate from runtime | Separate from runtime¹ |
| Offline plans · dry-run · JSON | Implemented | Implemented |
| Read-only discovery | Configured native directory | Configured native directory |
| Isolated hosted lifecycle (v0.1.0) | Passed: x64 / ARM64 | Passed: x64 / ARM64 |
| Real login-session acceptance | Pending | Pending |

¹ Enable before start/restart; stop before disabling a loaded job.

**Current scope:** user-account services on macOS and Linux. Windows, system/boot scope, built-in logs, secrets, external-service adoption, and automatic recovery are deferred or out of scope. See [implementation status](docs/IMPLEMENTATION.md) for the verification boundary.

<details>
<summary><strong>Safety and automation notes</strong></summary>

- Check both the JSON `ok` field and exit status; `doctor` also needs its nested state/session diagnostics checked.
- Native failures may follow partially successful commands. Inspect status before retrying; accepted commands do not prove a process remains running.
- Arguments and paths are plaintext. Do not put credentials in them. loboot does not sandbox the program you run.
- Filesystem updates are journaled; manager actions are not transactional. Follow the [recovery procedure](docs/TROUBLESHOOTING.md#interrupted-filesystem-change) instead of deleting a journal blindly.

</details>

## Go deeper

- **Use it:** [Installation](docs/INSTALL.md) · [Operator guide](docs/OPERATOR.md) · [Troubleshooting](docs/TROUBLESHOOTING.md)
- **Understand it:** [CLI contract](docs/CLI.md) · [Architecture](docs/ARCHITECTURE.md) · [Specification](docs/SPEC.md)
- **Build it:** [Contributing](CONTRIBUTING.md) · [Verification](docs/IMPLEMENTATION.md) · [Release process](docs/RELEASE.md)
- **Track it:** [Changelog](CHANGELOG.md) · [Security](docs/SECURITY.md) · [All documentation](docs/README.md)

### Looking for scheduled work?

[locron](https://github.com/WhiteKiwi/locron) schedules local commands and records their execution history. loboot manages native user services and their activation. Related tools, different jobs.

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Binary archives also include third-party notices.

## Agent skills

Assistant integrations are maintained separately in [WhiteKiwi/skills](https://github.com/WhiteKiwi/skills/tree/main/skills/loboot). Install the portable skill with the supported client selected interactively:

```sh
npx skills add WhiteKiwi/skills --skill loboot
```

[Claude Code, Codex, OpenClaw, and Hermes installation](https://github.com/WhiteKiwi/skills/blob/main/INSTALL.md) · [Versioned skill artifacts](https://github.com/WhiteKiwi/skills/releases/tag/loboot-v0.1.0) · [ClawHub](https://clawhub.ai/whitekiwi/skills/loboot).

The skill describes plans, consent boundaries, and CLI usage; it does not install the binary or grant permission to change services.
