# CI and release process

loboot separates everyday checks, artifact verification, and deliberate tag-triggered publication. The owner selected MIT OR Apache-2.0 and approved the experimental v0.1.0 GitHub/Homebrew distribution. Cargo remains `publish = false`; crates.io, signing/notarization, and new account credentials are not part of this release.

## Validation CI

[CI](../.github/workflows/ci.yml) runs on main pushes, pull requests, and manual dispatch:

- Pinned Rust 1.99.0 formatting and warnings-denied Clippy.
- Python packaging/formula tests and offline documentation-link/CLI-coverage checks.
- Rust 1.99.0 tests on Linux, plus stable Rust tests on Linux and macOS.
- Release-profile compilation and binary version/help smoke checks.

Actions are revision-pinned, checkout credentials are not persisted, permissions default to read-only, and jobs have timeouts. Ordinary tests never operate native services. Review checks for the exact commit, not only a badge or a previous revision.

## No-publication packaging rehearsal

[Release readiness](../.github/workflows/release-readiness.yml) runs for relevant pull-request changes, by manual dispatch, and as a reusable workflow. An optional tag is only a version-consistency check; this workflow never creates tags/releases.

| Runner | Native artifact target |
| --- | --- |
| Ubuntu 22.04 (`ubuntu-22.04`) | `x86_64-unknown-linux-gnu` |
| Ubuntu 22.04 ARM (`ubuntu-22.04-arm`) | `aarch64-unknown-linux-gnu` |
| macOS 15 | `aarch64-apple-darwin` |
| macOS 15 Intel | `x86_64-apple-darwin` |

Runner labels follow [GitHub's supported hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). This is the **unpublished candidate** matrix. Passing checks for the exact candidate establish build/install coverage only, not broader lifecycle support. Published v0.1.0 retains its historical Ubuntu 24.04 / glibc 2.39 baseline and unchanged bytes. Windows, musl/static builds, older Linux ABIs, and older macOS systems are not established.

Each target tests/builds natively. `scripts/release.py` validates Cargo/binary versions, the Rust host target, and actual ELF64/Mach-O64 architecture. It creates `loboot-vVERSION-TARGET.tar.gz` containing exactly:

- `loboot` (executable)
- `README.md` (standalone installation guide with source links)
- `LICENSE-STATUS.txt` (selected project license and complete texts)
- `THIRD-PARTY-LICENSES.txt` (locked target-filtered dependency notices, including build/dev dependencies conservatively)
- `RUST-COPYRIGHT.html` (the pinned toolchain's standard-library notice inventory)

The script rejects missing/unknown dependency license choices instead of assuming permission. Current dependencies offer MIT, with Unicode-3.0 additionally required by unicode-ident. Upstream texts are included without replacing their copyright holders.

Archives have normalized metadata and an exact regular-file inventory. Validation extracts only those files and runs the binary's version/help plus an offline JSON add dry-run with an empty PATH. The smoke confirms no root was created and no manager command was planned. This is not native lifecycle evidence or a reproducible-build claim.

The collection job requires exactly all four archives and validates their manifests/checksums. It emits `manifest.json` using `loboot.release/v1` and a combined `SHA256SUMS.txt`. Actions artifacts are retained for 14 days. The [implementation notes](IMPLEMENTATION.md) distinguish isolated native checks from fake/offline tests and pending logout/login acceptance.

### Linux candidate baseline (unreleased)

The two Linux packaging lanes use maintained, explicit Ubuntu 22.04 OS labels,
not `-latest`, with Rust **1.99.0**, locked Cargo dependencies, and explicitly set
Python 3.12 (the scripts require 3.11+). The runner labels are **not immutable
images**: GitHub updates their packages and image versions. Pinning the OS label
and Rust toolchain is not a byte-reproducibility guarantee.

After building and checking the strict archive, `scripts/linux_baseline.py`:

1. Requires the actual native Ubuntu 22.04 distribution, runtime glibc 2.35,
   matching CPU/Rust host target, and pinned Rust/Cargo versions. There is no
   cross-linker, QEMU, container runtime, musl target, or custom runner fallback.
2. Revalidates the exact `loboot.release/v1` bundle and ELF64 architecture, then
   copies only the verified archive's binary into a fresh temporary directory.
3. Requires GNU `readelf --version-info --wide`, `--dyn-syms --wide`, and
   `--dynamic --wide` output. Missing tooling, empty/malformed/truncated output,
   inconsistent version tables, unknown version namespaces (including
   `GLIBC_PRIVATE` / `GLIBC_ABI_*`), dynamic search-path overrides, and packed
   relocation requirements fail closed. GLIBC versions are compared numerically,
   including patch components, against the 2.35 ceiling.
4. Conservatively rejects **all** GLIBC references above 2.35, including weak
   references. Evidence preserves version-needs flags separately from dynamic
   symbol binding/section, so a weak reference is not described as a strong
   dependency or a proven hard runtime floor.
5. Runs the extracted binary's version/help and offline JSON dry-run on the
   native baseline with isolated HOME and empty PATH. This must pass on both
   Linux architectures for the exact candidate before claiming lower-baseline
   build/install coverage. Static inspection alone is insufficient.

The separate `loboot-linux-baseline-TARGET` Actions artifact records the source
commit, archive/binary hashes, distro, runtime glibc, CPU, runner label,
`ImageVersion`, Rust/Cargo/Python versions, host `cc`/`ld` diagnostics, raw inspection output,
command failures, and smoke outcome. Host `cc`/`ld` versions describe available
tools, not proof of the compiler/linker selected by Rust (which may use rust-lld).
It is uploaded even after a guard failure,
when a report exists, and retained for 14 days. It lives outside `dist`, so the
four archive names, exact inventory, checksums, and `loboot.release/v1` schema
remain unchanged. The inventory/publication jobs consume only `loboot-dist-*`.
A build failure before the guard may have logs but no baseline report; it is not
a baseline pass.

The separate opt-in disposable hosted lifecycle smoke is unchanged: a missing
existing user manager/session is a reported skip, **not a pass**, and no session
or linger settings are changed. Neither an offline smoke nor a hosted lifecycle
pass proves real logout/login activation. The recorded kernel is diagnostic
context, not a supported kernel minimum or a promise across all Linux systems.
Record revision-specific native runs, reported GLIBC maxima, runtime smoke
outcomes, and any remaining architecture gaps in [issue #14](https://github.com/WhiteKiwi/loboot/issues/14).
A link or static report alone does not satisfy the two-native-lane requirement.
This work does not publish a release or replace v0.1.0 assets.

### Local rehearsal

Python 3.11+, cached locked Cargo dependencies, and the pinned Rust toolchain are required. The additional Linux baseline check needs GNU binutils, a native compiler/linker, and an actual Ubuntu 22.04 / glibc 2.35 host:

```sh
cargo build --locked --release
python3 scripts/release.py build --target x86_64-unknown-linux-gnu --tag v0.1.0 --output dist/linux
python3 scripts/release.py check --target x86_64-unknown-linux-gnu --tag v0.1.0 --input dist/linux
python3 scripts/release.py gate --tag v0.1.0
# On native x64 Ubuntu 22.04 only; use the matching target/label on ARM64.
python3 scripts/linux_baseline.py --input dist/linux --target x86_64-unknown-linux-gnu \
  --runner-label ubuntu-22.04 --report /tmp/loboot-linux-baseline.json
```

Use the native target for the current machine; cross-linking is rejected. Outputs must be absent/empty so previous evidence is not overwritten.

## Automatic publication from an approved tag

[Release](../.github/workflows/release.yml) runs when an authorized `v*.*.*` tag is pushed. Main merges do not release. The workflow never chooses a version. An optional guarded manual entry creates the explicitly supplied tag only after all readiness checks pass.

The gate requires either a tag-push event or manual dispatch on main with an exact reviewed 40-character commit SHA. Both paths require a commit on main’s history, an exact stable Cargo version, chosen license texts, one nonempty versioned changelog entry, and no existing release. Manual dispatch refuses an existing tag unless `resume_existing_tag=true` is explicitly selected and that unpublished tag resolves to the exact same reviewed commit. It never moves a tag. All four native packaging lanes and collection must pass. Only the publication job receives ephemeral `contents: write`.

Publication revalidates the gate/inventory. Manual dispatch creates an annotated tag at the verified commit using the ephemeral repository token only after readiness, then publishes in the same run; it does not rely on a token-created tag generating another workflow. Both paths check that the tag resolves to the verified commit. It creates the release for the existing verified tag, using curated changelog notes, with seven assets: four archives, `SHA256SUMS.txt`, `manifest.json`, and generated `loboot.rb`. It then downloads every asset and compares the manifests, formula, and archive hashes. No crates, credentials, user-host installation, or service operations occur in the publication job.

Existing-release replacement is intentionally not automatic. Do not publish an empty UI release as a way to create the tag: it would conflict with the guarded publisher. Never move a published tag or overwrite published bytes to fix a release. Inspect a partial publication before taking any action.

## Homebrew distribution

`scripts/render_homebrew.py --manifest FILE --output FILE` renders the binary-only formula from the exact four-target combined manifest. It rejects unexpected targets, names, versions, duplicate targets, and malformed hashes. The formula has no `service` or installation hooks; its tests exercise version/help and offline diagnostics only.

Homebrew infers the version from the archive URLs. The published v0.1.0 formula asset contains a redundant explicit version line that Homebrew audit rejects. The tap removes only that exact line for v0.1.0 and otherwise verifies the complete formula against its expected rendering; future generated formulas omit it. Published assets and hashes remain unchanged.

[WhiteKiwi/homebrew-tap](https://github.com/WhiteKiwi/homebrew-tap) owns its updater and installation verification. The updater validates published assets and tests the candidate before changing its own formula using its existing ephemeral repository token. No cross-repository token is provisioned or copied from locron. The formula's livecheck is skipped because the verified updater owns its four-archive hashes. Skill integration lives separately in [WhiteKiwi/skills](https://github.com/WhiteKiwi/skills); skill publication does not install the CLI.

## Release checklist

1. Curate Cargo version and the matching changelog, license texts, platform limits, and dependency notices. Record native coverage honestly; real logout/login acceptance remains separate.
2. Merge only after exact-head CI and all four packaging jobs succeed, including both native Linux baseline reports with extracted-archive offline smoke. Verify the resulting main commit's checks. Keep hosted lifecycle skips/failures separate from ABI/install results.
3. Run the gate locally on the reviewed revision, then create the approved annotated tag and push only that tag:

   ```sh
   git fetch origin main
   git tag -a v0.1.0 APPROVED_COMMIT -m 'loboot v0.1.0'
   git push origin refs/tags/v0.1.0
   ```

   Replace `APPROVED_COMMIT` with the verified release commit; never tag an unreviewed working tree.
   Alternatively, run the **Release** workflow from main with `tag=v0.1.0` and `expected_commit=APPROVED_COMMIT`. If main has changed since review, the gate refuses and requires review of the new revision. This entry needs no new token or persistent access. If tag creation succeeded but no release exists, inspect the tag and failure first. A failed-publication-job rerun reuses only the exact matching tag. Alternatively, dispatch the same reviewed commit with `resume_existing_tag=true`; all gates/readiness rerun, and any mismatched tag or existing release is refused. A partially existing release requires manual inspection of published assets; this workflow never overwrites or deletes it.

4. Follow release to completion, verify tag/commit identity and seven public assets, download and check hashes, and safely smoke-test the extracted native binary.
5. Verify tap installation/tests and the assistant skill's independent release/install route. Announce only verified channels, with any remaining blocker stated.

macOS archives are unsigned/unnotarized. Checksums protect integrity but are not an independent publisher signature. The first release is experimental and does not claim real login/logout behavior merely because build, offline, or hosted lifecycle smoke checks pass.

## Opt-in macOS signing preparation

The separate [macOS signing candidate path](MACOS-SIGNING.md) prepares a guarded
manual workflow and credential-free command/integrity tests. It has no publication
permission and is never called by PR readiness or this release publisher. Owner
credential/account/environment setup and verifiable authorized signing/notarization
evidence remain prerequisites, not completed work. Standalone CLI binaries cannot
carry stapled tickets; fresh-machine Gatekeeper acceptance is separate. Existing
v0.1.0 artifacts and checksums remain unchanged and unsigned/unnotarized.
