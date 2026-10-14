# macOS signing and notarization candidates

This is an **opt-in preparation path**, not a claim that a signed release exists.
Published v0.1.0 archives remain unsigned/unnotarized, and their tag, bytes, and
checksums must not change. The [candidate workflow](../.github/workflows/macos-signing.yml)
does not create tags, releases, accounts, certificates, keys, or GitHub settings.
It does not install loboot or operate user services.

Actual Developer ID signing, Apple notarization, and fresh-machine Gatekeeper
acceptance are **unverified until an owner-authorized real run supplies evidence**.
Offline fixture tests prove command planning and refusal/integrity boundaries;
they do not prove that Apple accepted a real binary. Keep issue #13 open while
that evidence or its credential/account prerequisites remain unresolved.

## Artifact and trust model

The existing two native macOS targets and `.tar.gz` inventory are unchanged:
`aarch64-apple-darwin` and `x86_64-apple-darwin`. The workflow signs a fresh copy
of the exact reviewed binary. A SHA-256 chain records the reviewed source commit,
workflow run, unsigned archive, signed binary, submission ZIP, notarization
submission ID/CDHash, and final signed tarball. Signing changes binary bytes, so
the signed candidate has new checksums; never substitute it into an old release.

Apple accepts ZIP, UDIF disk images, and signed flat installer packages for
notarization, not tar.gz. We submit an exact one-file ZIP of the signed CLI, then
package those same signed bytes in the ordinary tarball. Mach-O carries this
CLI's signature inside the executable, so resource forks are not required.

Apple creates tickets for standalone binaries, but cannot staple tickets to
them. ZIP archives cannot be stapled either. Consequently this path records
`stapling: not-applicable-standalone-cli`, not a skipped failure or a successful
staple. Initial Gatekeeper verification may need internet access. A stapled DMG
or installer would be a separate distribution design, not an automatic addition.
See [Apple's custom notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

The signature must use an approved **Developer ID Application** identity, not an
ad-hoc, Apple Development, Mac Distribution, or Developer ID Installer identity.
Verification checks the certificate fingerprint, Apple trust chain and
Developer ID certificate OIDs, Team ID, stable code-signing identifier, hardened
runtime, secure timestamp, and binary CDHash. No entitlements or provisioning
profile are needed for this ordinary CLI's current capabilities. See
[Apple's distribution signing guide](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/)
and [code-signing requirements](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).

## Owner-controlled prerequisites and approval boundary

Do not put passwords, certificate private keys, or API private keys in issues,
PRs, chat, workflow inputs, repository files, or command examples. Preparing this
code is not permission to create credentials, store them in GitHub, expand
persistent access, enroll an account, accept agreements, or publish a release.
Any such setup needs separate owner authorization and the applicable secure
setup/approval mechanism. Existing credentials are not inspected automatically.

Before enabling a real run, the owner must establish and review:

1. The intended Apple Developer team and an eligible account/membership, a valid
   Developer ID Application certificate plus its private key, and authorization
   to use that identity for loboot. Apple documents the required account role in
   [Developer ID certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/).
2. An already authorized App Store Connect **team API key** for notarization,
   its Key ID and issuer UUID. This implementation intentionally supports one
   credential method only, not Apple ID passwords or persistent keychain
   credential profiles. Do not assume Apple Developer membership alone means
   the team's first notarization submission will be accepted.
3. An existing GitHub environment named `macos-signing`, restricted to the main
   branch, with required review and only the intended approvers. Configure its
   secrets there, not as repository/organization-wide signing secrets:
   - `MACOS_CERTIFICATE_BASE64`: Base64 of the existing exported `.p12` identity.
   - `MACOS_CERTIFICATE_PASSWORD`: that `.p12` file's password.
   - `MACOS_NOTARY_KEY`: the existing team API `.p8` private key.
4. Nonsecret repository variables identifying the intended identity:
   - `MACOS_SIGNING_TEAM_ID`: ten-character Apple Team ID.
   - `MACOS_SIGNING_IDENTITY_SHA1`: exact 40-hex certificate fingerprint.
   - `MACOS_SIGNING_IDENTIFIER`: stable reverse-domain code-signing identifier.
   - `MACOS_NOTARY_KEY_ID`: ten-character key ID.
   - `MACOS_NOTARY_ISSUER`: canonical issuer UUID.
5. Only after checking those protections, set the repository opt-in variable
   `MACOS_SIGNING_ENABLED` to the literal `true` through an approved setup action.

Merely writing `environment: macos-signing` in YAML does **not** configure access
protection. GitHub can automatically create a missing environment without
protection. Do not enable this workflow until the real environment is reviewed.
See [GitHub environment behavior](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)
and [secret handling](https://docs.github.com/en/actions/reference/security/secure-use).

No particular Xcode patch version is claimed as pinned. The hosted macOS labels
are fixed, but their image/tool versions receive updates. Retain the job's setup
logs when reviewing real evidence. If a particular Xcode is needed, select an
existing supported installation with `DEVELOPER_DIR`; do not change global
developer-tool settings or install an unreviewed toolchain as a workaround.

## Guarded no-publication workflow

The owner dispatches **macOS signed candidates (no publication)** from main,
supplying the exact reviewed main SHA and explicitly checking the authorization
to sign that revision and upload its CLI binaries to Apple's notary service.
Notarization uploads code to Apple and creates a submission record. “No
publication” does not mean side-effect-free or read-only.

The jobs deliberately separate trust boundaries:

1. **Gate:** validates the repository, manual event, main ref, opt-in, explicit
   upload authorization, exact commit, version, and nonsecret identity fields.
   A failed gate stops before environment access or signing credentials.
2. **Build:** tests/builds each native target on a disposable hosted runner with
   Rust 1.99.0 and locked dependencies. Ordinary packaging smoke is credential
   free. Artifacts are bound to the run/commit/target and downloaded only from
   this same run; there are no user-supplied run IDs or cross-run artifact URLs.
3. **Sign:** a separate fresh runner and protected-environment job validates the
   archive without executing its binary. Only the signing step receives the
   three secrets. It creates a temporary private keychain, imports the identity
   for codesign, signs a private binary copy, submits the ZIP, and verifies
   explicit `Accepted` status plus matching log ID/ZIP hash/native CDHash.
   Warnings require review rather than being silently accepted. It never runs
   Cargo, a downloaded executable, installation hooks, or native service tools.
4. **Verify:** a fresh credential-free native runner independently checks the
   archived signature/notarization and executes version/help plus the existing
   offline JSON dry-run with an empty PATH and isolated HOME. No native service
   lifecycle action is part of this signed-candidate workflow.

Private material is kept outside artifact directories, is not passed through to
child environments, and is cleaned up on ordinary failures and cancellation.
Hosted-runner destruction is the final cleanup boundary for abrupt termination.
No default keychain/search-list settings or persistent notary profiles change.
Commands do not log their secret arguments or unfiltered failure output.
See [GitHub's temporary-keychain guidance](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications).

PR CI and the reusable release-readiness workflow continue to receive **no
signing secrets**. There is no privileged PR trigger, shared signing cache, or
automatic retry of an Apple submission. All Actions references remain pinned.

## Evidence, pending submissions, and local checks

Each `macos-signed-candidate-TARGET` artifact contains:

- `bundle/`: exactly the ordinary tarball, `manifest.json`, and `SHA256SUMS.txt`.
  Its five archive members and release/v1 manifest remain compatible. Only the
  signed binary and evidence-driven README differ from the unsigned input; all
  license/runtime notices are preserved. README links identify the reviewed
  commit, not the moving main branch or the old v0.1.0 tag.
- `evidence/`: input identity, submission status, exact submitted ZIP, Apple's
  notarization log, and a successful result only after matching verification.
- A separate `macos-signing-verification-TARGET` artifact records the fresh
  native trust/offline-smoke result. Candidate existence alone is not a full
  workflow pass. All evidence artifacts have a 14-day retention period.

Evidence is JSON for review and hash consistency, not a separately signed source
attestation. Trust the exact repository/run/commit and Apple's native signature
and notarization checks, not an arbitrary JSON file claiming success.

If submission times out before an ID is returned, evidence says
`submission-outcome-unknown`. If waiting times out after an ID was saved, retain
that ID and exact ZIP. An owner-authorized investigation must query that existing
submission before considering a retry. `In Progress`, `Invalid`, missing logs,
or network failures are not success and never authorize publication. The workflow
does not resume or resubmit automatically. Do not delete or overwrite a partial
submission's evidence to make a run appear clean.

For credential-free local integrity checking of downloaded candidate evidence:

```sh
python3 scripts/macos_signing.py verify \
  --bundle CANDIDATE/bundle --evidence CANDIDATE/evidence \
  --target aarch64-apple-darwin --version VERSION \
  --commit REVIEWED_SHA --run-id VERIFIED_RUN_ID \
  --team-id TEAM_ID --identity CERTIFICATE_SHA1 --identifier CODE_IDENTIFIER \
  --key-id NOTARY_KEY_ID --issuer NOTARY_ISSUER_UUID
```

Replace all placeholders with the expected nonsecret values from the trusted
run. On the matching native Mac, with the pinned Rust toolchain available, add
`--native` to independently check the exact leaf certificate, Developer ID
requirements, notarization and extracted CLI smoke. Without `--native`, the
result explicitly says native verification was not run. Native trust can also
be inspected directly:

```sh
codesign --verify --strict --verbose=2 ./loboot
codesign --display --verbose=4 ./loboot
codesign --verify --strict --verbose=4 -R='notarized' --check-notarization ./loboot
```

These command-line checks do not replace fresh-machine distribution testing.
For actual Gatekeeper acceptance, use a fresh isolated Mac/VM, download using a
quarantining mechanism such as Safari, and follow the normal installation path.
Keep the network available for the unstapled CLI. Do not disable Gatekeeper,
delete quarantine metadata, or bypass a security warning to produce a pass.
Follow [Apple's notarized-product testing procedure](https://developer.apple.com/forums/thread/130560).

## Publication remains separate

The existing release workflow never consumes the candidate artifact names and
does not call this workflow. All jobs have only `contents: read`; none create a
tag, GitHub Release, Homebrew change, or upload to a distribution channel.

Before any future signed release: finish authorized real evidence for both
architectures, review Gatekeeper coverage, choose a new version, and deliberately
integrate verified signed-artifact collection/publication with the four-platform
release gates. That integration and release need review/authorization; this
preparation must not silently label ordinary unsigned release artifacts signed.
