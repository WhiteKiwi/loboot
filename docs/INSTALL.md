# Installation

loboot v0.1.0 is experimental and licensed under MIT OR Apache-2.0. Install from the [v0.1.0 GitHub Release](https://github.com/WhiteKiwi/loboot/releases/tag/v0.1.0), the [WhiteKiwi Homebrew tap](https://github.com/WhiteKiwi/homebrew-tap), or source. No crates.io release, shell installer, signed/notarized macOS download, or Windows package is offered.

## Homebrew

```sh
brew install WhiteKiwi/tap/loboot
loboot --version
loboot --help
```

Use `brew upgrade loboot` to update and `brew uninstall loboot` to remove the binary. The formula has no service stanza or installation hooks; `brew services start loboot` is not supported because loboot is not a daemon. Homebrew installation does not start or enable managed jobs.

## Download a release archive

Use the exact version page at [GitHub Releases](https://github.com/WhiteKiwi/loboot/releases), and download `SHA256SUMS.txt` plus the archive matching your system:

| Platform | Target | Tested build baseline |
| --- | --- | --- |
| Linux x64 | `x86_64-unknown-linux-gnu` | Ubuntu 24.04, glibc 2.39 |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | Ubuntu 24.04, glibc 2.39 |
| Apple Silicon | `aarch64-apple-darwin` | macOS 15 |
| Intel Mac | `x86_64-apple-darwin` | macOS 15 |

The archive name is `loboot-vVERSION-TARGET.tar.gz`. Verify its entry in the checksum file using `sha256sum` (Linux) or `shasum -a 256` (macOS) before extracting. Checksums detect altered bytes but are not an independent publisher signature. Extract into a fresh directory, inspect the included licenses, run `./loboot --version` and `./loboot --help`, then copy the binary into a directory on your PATH such as `$HOME/.local/bin`.

macOS downloads are unsigned/unnotarized. If Gatekeeper blocks execution, do not bypass a security warning automatically; prefer the reviewed source build or ask your administrator. Older macOS/Linux versions and alternative libc distributions have not been established by the release matrix. Native runtime requirements still apply.

A separate [opt-in signing preparation path](MACOS-SIGNING.md) exists for reviewed
unpublished candidates. It does not change the trust status of v0.1.0 or offer a
signed download. Real authorized signing/notarization evidence is still required.

## Requirements

- For source builds: Rust 1.99.0 through Rustup; `rust-toolchain.toml` pins the tested toolchain and development components.
- For source builds: Git and a native compiler/linker for your platform.
- For real Linux operations: systemd 240+ with `systemctl` on a trusted `PATH` and a reachable user manager. Minimal containers and SSH sessions may not have one.
- For real macOS operations: `/bin/launchctl` and the current account's graphical `gui/<uid>` session.
- Existing absolute executable and working-directory paths without symlink components. Use physical paths such as `/private/tmp` on macOS.

Offline rendering and tests do not require a live service manager. The binary matrix above is build/install coverage, not proof of logout/login activation.

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
