# CI and release process

loboot uses locron's separation of everyday checks, artifact verification, and publication as a reference. It does not inherit locron's package availability, license, signing identity, registry access, or supported platforms.

**Current state:** source-build only, license undecided, Cargo `publish = false`, no public package release. A guarded manual GitHub release workflow is configured but has not been invoked. Packaging checks and uploaded Actions artifacts are not completed release/CD acceptance.

## 1. Validation CI

[`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on main pushes, pull requests, and manual dispatch:

- Pinned Rust 1.99.0 formatting and warnings-denied Clippy.
- Python release-script tests and offline documentation-link/CLI-coverage checks.
- Rust 1.99.0 tests on Linux, plus stable Rust tests on Linux and macOS.
- Release-profile compilation and binary `--version`/`--help` smoke checks.

Actions are revision-pinned, checkout credentials are not persisted, permissions default to read-only, and jobs have timeouts. No check registers or changes a real native service. Review checks for the **exact commit**, not only a badge or a previous revision. See [verification evidence](IMPLEMENTATION.md).

## 2. No-publication packaging rehearsal

[`.github/workflows/release-readiness.yml`](../.github/workflows/release-readiness.yml) runs for relevant pull-request changes, by manual dispatch, and as a reusable workflow. A supplied `tag` is only a version-consistency check; this workflow never creates tags or releases.

The current native build targets are exactly:

| Runner | Artifact target |
| --- | --- |
| Ubuntu 24.04 | `x86_64-unknown-linux-gnu` |
| macOS 15 | `aarch64-apple-darwin` |

This is the packaging rehearsal matrix, not a claim of fully accepted support. Intel macOS, Linux ARM, Windows, static/musl builds, and broader Linux ABI compatibility are not established here.

Each target tests and builds on its native runner, then `scripts/release.py`:

1. Checks the Cargo/binary version, Rust host target, and actual ELF64/Mach-O64 executable architecture.
2. Builds `loboot-vVERSION-TARGET.tar.gz` containing the executable, a compact installation README with absolute source-documentation links, and `LICENSE-STATUS.txt`. Pending licensing is stated explicitly in candidate archives.
3. Validates the exact archive inventory, regular-file types, permissions, and normalized metadata. It extracts only the expected regular files.
4. Runs the extracted binary's version/help checks and an offline JSON add dry-run with an empty `PATH`. The smoke check verifies no offline root was created and no native command was planned.
5. Produces and verifies SHA-256 checksums and a `loboot.release/v1` manifest.

A collection job requires exactly both target archives and validates their manifests/checksums before producing a combined bundle. Actions artifacts are retained for 14 days. These are unsigned, unnotarized candidates; no signing credentials are required or provisioned for rehearsal. Deterministic archive metadata does not imply reproducible Rust binary builds.

### Local rehearsal

Use Python 3.11+ and the pinned Rust toolchain. On a matching Linux x86_64 host:

```sh
cargo build --locked --release
python3 scripts/release.py build --target x86_64-unknown-linux-gnu --output dist/linux
python3 scripts/release.py check --target x86_64-unknown-linux-gnu --input dist/linux
```

On an Apple Silicon macOS host use `aarch64-apple-darwin` and a separate output directory. The selected target must match `rustc -vV`; this script does not cross-compile. Output must be absent or empty. Preserve a prior candidate rather than overwriting its evidence.

The `build`, `check`, `collect`, and `gate` subcommands never publish. `gate --tag v0.1.0` currently refuses because no license has been selected; the Unreleased changelog also must become an approved versioned release entry before publication.

## 3. Guarded public GitHub release workflow

[`.github/workflows/release.yml`](../.github/workflows/release.yml) is **manual only**. It has no tag-push publication trigger and does not create a tag. An authorized release operator must dispatch it against an existing `vX.Y.Z` tag with `publish=true`.

Automated prerequisites are:

- Explicit `publish=true` and an existing tag ref rather than a branch.
- The tagged commit is an ancestor of `origin/main`.
- The exact tag matches the stable `X.Y.Z` Cargo version.
- Cargo selects one nonempty `license` or supported `license-file` field and corresponding nonempty license text exists.
- `CHANGELOG.md` contains a level-two entry for that exact version.
- Both native packaging jobs, extracted-binary smoke checks, and exact inventory/checksum collection pass on the selected revision.

Only the final publication job receives `contents: write`; it uses GitHub's ephemeral workflow token. It rechecks release prerequisites and artifact inventory, then creates a public GitHub release for the verified existing tag with two archives, `SHA256SUMS.txt`, and `manifest.json`. It does not publish crates, modify a Homebrew tap, install binaries, operate services, or provision signing credentials. Existing-release replacement and resumable publication are not implemented; inspect any partial publication before deciding what to do next.

License fields are an automated presence gate, not a legal review. Native lifecycle approval and the suitability of distribution are **operator decisions**, not inferred from the packaging job.

## 4. Before approving the first public release

The repository owner/release operator must:

1. Choose a license and review its text/metadata. Do not copy locron's license by assumption.
2. Complete and record [isolated native lifecycle/login acceptance](IMPLEMENTATION.md#opt-in-isolated-native-validation-protocol-pending), or explicitly decide and disclose the restricted experimental release scope. Do not label untested lifecycle behavior accepted.
3. Approve the release version, changelog, channels, and platform claims. Keep unsupported architectures and unsigned/unnotarized status clear.
4. Verify exact-revision CI and no-publication rehearsal evidence. Confirm archive contents, version, checksums, and extracted-binary behavior.
5. Approve creation of the intended tag and public release separately from a PR merge. Only then select that tag and dispatch publication with the explicit flag.
6. After publication, verify the release resolves to the approved tag/commit, inventory and downloaded hashes match, and an extracted download passes the safe smoke check. Do not announce distribution before this verification.

No tag or public release is created by ordinary CI or by merely adding these workflows. Removing `publish = false`, adding installers/package managers, expanding platforms, or adding signing/notarization needs a separate reviewed implementation and release decision. Checksums detect corruption/changed bytes; they do not independently authenticate the publisher.
