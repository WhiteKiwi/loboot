# Installation

loboot is currently **source-build only** on macOS and Linux. No crates.io release, Homebrew formula, shell installer, signed macOS download, or Windows package is offered. The repository owner has not selected a license; no license grant is implied by public access.

## Requirements

- Rust 1.99.0 through Rustup; `rust-toolchain.toml` pins the tested toolchain and development components.
- Git and a native compiler/linker for your platform.
- For real Linux operations: systemd 240+ with `systemctl` on a trusted `PATH` and a reachable user manager. Minimal containers and SSH sessions may not have one.
- For real macOS operations: `/bin/launchctl` and the current account's graphical `gui/<uid>` session.
- Existing absolute executable and working-directory paths without symlink components. Use physical paths such as `/private/tmp` on macOS.

Offline rendering and tests do not require a live service manager. No supported architecture matrix or minimum macOS release has been established by native lifecycle acceptance yet.

## Install from this checkout

```sh
git clone https://github.com/WhiteKiwi/loboot.git
cd loboot
cargo install --path . --locked
loboot --version
loboot --help
```

Cargo normally installs into `$HOME/.cargo/bin` unless you override its install root. Add the actual bin directory to your shell's `PATH` if needed. Do not run installation or loboot with `sudo` for user-service management.

To build without installing:

```sh
cargo build --locked --release
./target/release/loboot --help
```

For dependency-cache-only work, append `--offline` to Cargo commands. An empty cache cannot build offline; that is a dependency-availability problem, not a reason to remove the lockfile.

The binary has no installer hooks. Installing or building it does not register a job, start a daemon, change login settings, or alter systemd lingering.

## Update a source installation

Review and check out the intended revision, then reinstall from that checkout:

```sh
cargo install --path . --locked --force
loboot --version
```

Here `--force` is Cargo's binary replacement option; loboot has no force-adoption mode. A binary update does not change stored definitions or restart managed applications. Keep a backup of definitions and state, and review compatibility changes before upgrading an early v0.1 installation. The version alone does not identify an unreleased commit; retain its Git revision too.

## Uninstall

Decide separately whether to retain managed services. Removing the loboot binary leaves their native definitions and activation preferences in place; the operating system can continue running them without loboot.

For each service you intend to remove, while loboot is still installed:

```sh
loboot list
loboot stop SERVICE_ID
loboot disable SERVICE_ID
loboot remove SERVICE_ID --dry-run
loboot remove SERVICE_ID
```

Replace `SERVICE_ID` with an exact managed ID. Investigate errors instead of deleting unrelated native files or whole native directories. Then remove a Cargo-installed binary:

```sh
cargo uninstall loboot
```

For a manually copied binary, remove only that copy. Retained state and `.lock` files are not an active daemon. Keep recovery journals and diagnostic evidence until any interrupted operation is resolved. See [state locations](OPERATOR.md#state-and-native-locations) and [recovery](TROUBLESHOOTING.md#interrupted-filesystem-change).
