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
| Ubuntu 24.04 | `x86_64-unknown-linux-gnu` |
| Ubuntu 24.04 ARM | `aarch64-unknown-linux-gnu` |
| macOS 15 | `aarch64-apple-darwin` |
| macOS 15 Intel | `x86_64-apple-darwin` |

Runner labels follow [GitHub's supported hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). These are tested build/install baselines, not broader lifecycle support claims. Linux builds require glibc 2.39 or newer. Windows, musl/static builds, older Linux ABIs, and older macOS systems are not established.

Each target tests/builds natively. `scripts/release.py` validates Cargo/binary versions, the Rust host target, and actual ELF64/Mach-O64 architecture. It creates `loboot-vVERSION-TARGET.tar.gz` containing exactly:

- `loboot` (executable)
- `README.md` (standalone installation guide with source links)
- `LICENSE-STATUS.txt` (selected project license and complete texts)
- `THIRD-PARTY-LICENSES.txt` (locked target-filtered dependency notices, including build/dev dependencies conservatively)
- `RUST-COPYRIGHT.html` (the pinned toolchain's standard-library notice inventory)

The script rejects missing/unknown dependency license choices instead of assuming permission. Current dependencies offer MIT, with Unicode-3.0 additionally required by unicode-ident. Upstream texts are included without replacing their copyright holders.

Archives have normalized metadata and an exact regular-file inventory. Validation extracts only those files and runs the binary's version/help plus an offline JSON add dry-run with an empty PATH. The smoke confirms no root was created and no manager command was planned. This is not native lifecycle evidence or a reproducible-build claim.

The collection job requires exactly all four archives and validates their manifests/checksums. It emits `manifest.json` using `loboot.release/v1` and a combined `SHA256SUMS.txt`. Actions artifacts are retained for 14 days. The [implementation notes](IMPLEMENTATION.md) distinguish isolated native checks from fake/offline tests and pending logout/login acceptance.

### Local rehearsal

Python 3.11+, cached locked Cargo dependencies, and the pinned Rust toolchain are required:

```sh
cargo build --locked --release
python3 scripts/release.py build --target x86_64-unknown-linux-gnu --tag v0.1.0 --output dist/linux
python3 scripts/release.py check --target x86_64-unknown-linux-gnu --tag v0.1.0 --input dist/linux
python3 scripts/release.py gate --tag v0.1.0
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
2. Merge only after exact-head CI and all four packaging jobs succeed. Verify the resulting main commit's checks.
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
