#!/usr/bin/env python3
"""Plan or explicitly prepare macOS signed candidates. Never publish a release.

The plan and input-binding commands do not access credentials or run Apple tools.
Execution is restricted to an explicitly enabled, approved main-branch dispatch
on a disposable GitHub-hosted runner. See docs/MACOS-SIGNING.md before enabling.
"""
from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import tarfile
import tempfile
import uuid
import zipfile

import release


SCHEMA = "loboot.macos-signing/v1"
REPOSITORY = "WhiteKiwi/loboot"
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
SECRET_NAMES = ("MACOS_CERTIFICATE_BASE64", "MACOS_CERTIFICATE_PASSWORD", "MACOS_NOTARY_KEY")
UNSIGNED_NOTICE = "These candidates do not claim macOS signing or\nnotarization."


class SigningError(ValueError):
    """Fail closed without exposing credentials or unfiltered command output."""


def json_object(payload: str | bytes) -> dict:
    def pairs(entries):
        result = {}
        for key, value in entries:
            if key in result:
                raise SigningError("JSON contains duplicate fields")
            result[key] = value
        return result
    def constant(value):
        raise SigningError("JSON contains a nonfinite value")
    value = json.loads(payload, object_pairs_hook=pairs, parse_constant=constant)
    if not isinstance(value, dict):
        raise SigningError("expected a JSON object")
    return value


def matching(value: str, expression: str, name: str) -> str:
    if not isinstance(value, str) or re.fullmatch(expression, value) is None:
        raise SigningError(f"invalid {name}")
    return value


def context(target: str, version: str, commit: str, run_id: str) -> dict:
    if target not in TARGETS:
        raise SigningError("signing requires one of the two native macOS targets")
    release.validate_version(version)
    matching(commit, r"[0-9a-f]{40}", "reviewed commit")
    matching(run_id, r"[1-9][0-9]*", "workflow run ID")
    return {"schema_version": SCHEMA, "repository": REPOSITORY,
            "commit": commit, "run_id": run_id, "target": target, "version": version}


def signing_identity(team: str, identity: str, identifier: str, key_id: str, issuer: str) -> dict:
    matching(team, r"[A-Z0-9]{10}", "Team ID")
    matching(identity, r"[A-Fa-f0-9]{40}", "certificate SHA-1 fingerprint")
    matching(identifier, r"[A-Za-z0-9]+(?:[.-][A-Za-z0-9]+)+", "code-signing identifier")
    matching(key_id, r"[A-Z0-9]{10}", "notarization key ID")
    if str(uuid.UUID(issuer)) != issuer:
        raise SigningError("invalid notarization issuer UUID")
    return {"team_id": team, "identity_sha1": identity.upper(), "identifier": identifier,
            "notary_key_id": key_id, "notary_issuer": issuer}


def input_identity(bundle: Path, ctx: dict) -> dict:
    if bundle.is_symlink() or not bundle.is_dir():
        raise SigningError("input bundle must be a directory, not a link")
    metadata = release.verify_bundle(bundle, ctx["version"], ctx["target"], execute=False)
    return {**ctx, "unsigned_archive": metadata["archive"], "unsigned_sha256": metadata["sha256"]}


def bind_input(bundle: Path, ctx: dict, output: Path) -> dict:
    value = input_identity(bundle, ctx)
    with output.open("x", encoding="utf-8") as destination:
        destination.write(json.dumps(value, indent=2, sort_keys=True) + "\n")
    return value


def verify_input(bundle: Path, provenance: Path, ctx: dict) -> dict:
    value = input_identity(bundle, ctx)
    if json_object(release.regular_file(provenance)) != value:
        raise SigningError("input provenance does not match this run, commit, target, or archive")
    return value


def requirement(identity: dict) -> str:
    return ('anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
            'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
            f'and certificate leaf[subject.OU] = "{identity["team_id"]}" '
            f'and identifier "{identity["identifier"]}"')


def commands(identity: dict, binary: Path, keychain: Path, key: Path, archive: Path) -> dict:
    """Pure argv construction. Never embed a credential value in a plan."""
    auth = ["--key", str(key), "--key-id", identity["notary_key_id"], "--issuer", identity["notary_issuer"]]
    return {
        "sign": ["/usr/bin/codesign", "--sign", identity["identity_sha1"], "--force", "--timestamp", "--options", "runtime",
                 "--identifier", identity["identifier"], "--keychain", str(keychain),
                 str(binary)],
        "verify_signature": ["/usr/bin/codesign", "--verify", "--strict", "--verbose=2",
                             "-R", "=" + requirement(identity), str(binary)],
        "inspect_signature": ["/usr/bin/codesign", "--display", "--verbose=4", str(binary)],
        "submit": ["/usr/bin/xcrun", "notarytool", "submit", str(archive), *auth, "--output-format", "json"],
        "wait": ["/usr/bin/xcrun", "notarytool", "wait", "SUBMISSION_ID", *auth, "--output-format", "json"],
        "log": ["/usr/bin/xcrun", "notarytool", "log", "SUBMISSION_ID", *auth, "NOTARY_LOG_PATH"],
        "verify_notarization": ["/usr/bin/codesign", "--verify", "--strict", "--verbose=4",
                                "-R", f'=({requirement(identity)}) and notarized', "--check-notarization", str(binary)],
    }


def plan(bundle: Path, provenance: Path, ctx: dict, identity: dict) -> dict:
    source = verify_input(bundle, provenance, ctx)
    return {**source, "status": "planned", "signing": identity, "publication": "forbidden",
            "stapling": "not-applicable-standalone-cli", "requires_credentials": list(SECRET_NAMES),
            "notarization_zip": {"members": ["loboot"], "preserve_signed_bytes": True},
            "commands": commands(identity, Path("STAGING/loboot"), Path("PRIVATE/signing.keychain-db"),
                                 Path("PRIVATE/notary.p8"), Path("EVIDENCE/submission.zip"))}


def require_execution(ctx: dict, env: dict) -> None:
    expected = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                "GITHUB_REPOSITORY": REPOSITORY, "GITHUB_EVENT_NAME": "workflow_dispatch",
                "GITHUB_REF": "refs/heads/main", "GITHUB_SHA": ctx["commit"],
                "GITHUB_RUN_ID": ctx["run_id"], "LOBOOT_SIGNING_ENABLED": "true",
                "LOBOOT_SIGNING_APPROVED": "true"}
    if any(env.get(key) != value for key, value in expected.items()):
        raise SigningError("execution requires enabled, explicitly approved exact-main GitHub-hosted dispatch")
    if sys.platform != "darwin":
        raise SigningError("execution requires native macOS")
    if any(not env.get(key) for key in SECRET_NAMES):
        raise SigningError("missing signing credentials; nothing was signed or submitted")


def invoke(argv: list[str], *, timeout: int = 60, allow_failure: bool = False) -> str:
    # Signing inputs are explicitly supplied. Do not pass repository, artifact, or
    # certificate secrets through to arbitrary child processes or binary smoke.
    env = {name: os.environ[name] for name in ("HOME", "TMPDIR", "DEVELOPER_DIR") if name in os.environ}
    env.update(PATH="/usr/bin:/bin:/usr/sbin:/sbin", LC_ALL="C")
    try:
        result = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        raise SigningError(f"{Path(argv[0]).name} timed out; inspect saved submission evidence before any retry") from error
    if result.returncode and not allow_failure:
        # Do not echo argv/stderr: security/notary errors may include secrets.
        raise SigningError(f"{Path(argv[0]).name} failed with exit {result.returncode}; no publication occurred")
    return result.stdout + result.stderr if argv[0] == "/usr/bin/codesign" else result.stdout


def signature_details(output: str, identity: dict) -> dict:
    def one(pattern: str, label: str) -> str:
        values = re.findall(pattern, output, re.MULTILINE)
        if len(values) != 1:
            raise SigningError(f"signature has missing or ambiguous {label}")
        return values[0]
    team = one(r"^TeamIdentifier=(\S+)$", "Team ID")
    identifier = one(r"^Identifier=(\S+)$", "identifier")
    cdhash = one(r"^CDHash=([a-f0-9]{40})$", "CDHash")
    timestamp = one(r"^Timestamp=(.+)$", "secure timestamp")
    if team != identity["team_id"] or identifier != identity["identifier"]:
        raise SigningError("signature Team ID or identifier mismatch")
    if not re.search(r"^CodeDirectory .*flags=0x[0-9a-f]+\([^\n]*\bruntime\b", output, re.MULTILINE):
        raise SigningError("signature lacks hardened runtime")
    if not re.search(r"^Authority=Developer ID Application: .+$", output, re.MULTILINE):
        raise SigningError("signature lacks Developer ID Application authority")
    return {"team_id": team, "identifier": identifier, "cdhash": cdhash, "timestamp": timestamp}


def inspect_signature(binary: Path, identity: dict) -> dict:
    details = signature_details(invoke(["/usr/bin/codesign", "--display", "--verbose=4", str(binary)]), identity)
    with tempfile.TemporaryDirectory(prefix="loboot-public-certificate-") as temporary:
        prefix = Path(temporary) / "certificate"
        invoke(["/usr/bin/codesign", "--display", f"--extract-certificates={prefix}", str(binary)])
        fingerprint = hashlib.sha1(release.regular_file(Path(f"{prefix}0"))).hexdigest().upper()
        if fingerprint != identity["identity_sha1"]:
            raise SigningError("actual leaf certificate differs from approved fingerprint")
    return details


def submission_id(response: dict) -> str:
    if not isinstance(response, dict):
        raise SigningError("notary response must be an object")
    value = response.get("id")
    if not isinstance(value, str) or str(uuid.UUID(value)) != value:
        raise SigningError("notary service returned no canonical submission ID; outcome may be uncertain")
    return value


def verify_notary_log(log: dict, identifier: str, archive_sha: str, cdhash: str, target: str) -> None:
    architecture = "arm64" if target.startswith("aarch64") else "x86_64"
    if (not isinstance(log, dict) or log.get("jobId") != identifier or log.get("status") != "Accepted"
            or log.get("sha256") != archive_sha or type(log.get("statusCode")) is not int or log.get("statusCode") != 0):
        raise SigningError("notarization log is not Accepted for this submission and ZIP hash")
    tickets = log.get("ticketContents")
    if not isinstance(tickets, list) or not any(
        isinstance(ticket, dict) and ticket.get("cdhash") == cdhash
        and ticket.get("arch") == architecture
        and isinstance(ticket.get("path"), str)
        and ticket["path"].split("/")[-1] == "loboot" for ticket in tickets
    ):
        raise SigningError("notarization ticket does not contain this native binary CDHash")
    if log.get("issues") not in (None, []):
        raise SigningError("notarization reported issues; inspect the saved log before accepting the candidate")


def contents_from_bundle(bundle: Path, ctx: dict) -> dict[str, bytes]:
    name = release.archive_stem(ctx["version"], ctx["target"])
    # Caller already validated exact regular-file inventory; repeat before use.
    release.verify_bundle(bundle, ctx["version"], ctx["target"], execute=False)
    with tarfile.open(bundle / f"{name}.tar.gz", "r:gz") as archive:
        return {filename: archive.extractfile(f"{name}/{filename}").read() for filename in release.ARCHIVE_FILES}


def submission_zip(binary: Path, output: Path) -> None:
    # The CLI signature is embedded in Mach-O, not in resource forks. An exact
    # one-file ZIP avoids incidental Finder metadata and never alters its bytes.
    member = zipfile.ZipInfo("loboot")
    member.create_system = 3
    member.external_attr = 0o100755 << 16
    member.compress_type = zipfile.ZIP_DEFLATED
    with zipfile.ZipFile(output, "x") as archive:
        archive.writestr(member, release.regular_file(binary))


def signed_readme(content: bytes, details: dict, commit: str) -> bytes:
    text = content.decode("utf-8")
    if text.count(UNSIGNED_NOTICE) != 1:
        raise SigningError("archive README lacks the exact unsigned notice; review before replacing it")
    notice = (f'This unpublished candidate was signed with Developer ID Application for team {details["team_id"]}\n'
              f'and notarized by Apple (submission {details["submission_id"]}). The standalone\n'
              'CLI cannot carry a stapled ticket; first-launch verification may need internet.\n'
              'See the separately supplied signing evidence. This is not a published release or\n'
              'proof of fresh-machine Gatekeeper acceptance.')
    text = text.replace(UNSIGNED_NOTICE, notice)
    text = text.replace(f"{release.SOURCE_REPOSITORY}/blob/main/", f"{release.SOURCE_REPOSITORY}/blob/{commit}/")
    text = text.replace("the main branch; a candidate may differ from main", "the reviewed signing commit")
    return text.encode("utf-8")


def write_candidate(contents: dict, binary: Path, output: Path, ctx: dict, details: dict) -> dict:
    payload = release.regular_file(binary)
    if hashlib.sha256(payload).hexdigest() != details["signed_binary_sha256"]:
        raise SigningError("signed binary changed after notarization")
    release.require_binary_header(payload[:64], ctx["target"])
    contents = {**contents, "loboot": payload, "README.md": signed_readme(contents["README.md"], details, ctx["commit"])}
    output.mkdir()
    archive = output / f'{release.archive_stem(ctx["version"], ctx["target"])}.tar.gz'
    release.write_archive(archive, release.archive_stem(ctx["version"], ctx["target"]), contents)
    metadata = release.bundle_metadata(ctx["version"], ctx["target"], archive)
    release.dump_json(output / "manifest.json", metadata)
    (output / "SHA256SUMS.txt").write_text(f'{metadata["sha256"]}  {archive.name}\n', encoding="utf-8")
    release.verify_bundle(output, ctx["version"], ctx["target"], execute=False)
    return metadata


def execute(bundle: Path, provenance: Path, output: Path, ctx: dict, identity: dict) -> dict:
    # Validate all nonsecret inputs and all credentials before a keychain, tool,
    # output, network submission, or signing operation can be created.
    source = verify_input(bundle, provenance, ctx)
    require_execution(ctx, os.environ)
    release.require_empty_output(output)
    if output.is_symlink():
        raise SigningError("output must not be a link")
    try:
        certificate = base64.b64decode(os.environ[SECRET_NAMES[0]], validate=True)
    except (ValueError, binascii.Error) as error:
        raise SigningError("certificate is not valid Base64") from error
    if not certificate or "-----BEGIN PRIVATE KEY-----" not in os.environ[SECRET_NAMES[2]]:
        raise SigningError("certificate or notarization key is empty/invalid")
    certificate_password = os.environ[SECRET_NAMES[1]]
    notary_key = os.environ[SECRET_NAMES[2]]
    for name in SECRET_NAMES:
        os.environ.pop(name, None)
    contents = contents_from_bundle(bundle, ctx)
    signed_readme(contents["README.md"], {"team_id": identity["team_id"], "submission_id": "pending"}, ctx["commit"])
    output.mkdir(parents=True, exist_ok=True)
    evidence = output / "evidence"
    evidence.mkdir()
    release.dump_json(evidence / "source.json", source)
    # Private material never lives under the artifact/output directory.
    with tempfile.TemporaryDirectory(prefix="loboot-signing-") as temporary:
        private = Path(temporary).resolve()
        private.chmod(0o700)
        keychain, key = private / "signing.keychain-db", private / "notary.p8"
        cert, binary = private / "certificate.p12", private / "loboot"
        for path, payload in ((cert, certificate), (key, notary_key.encode()), (binary, contents["loboot"])):
            with path.open("xb") as destination:
                destination.write(payload)
            path.chmod(0o700 if path == binary else 0o600)
        password = secrets.token_hex(32)
        argv = commands(identity, binary, keychain, key, evidence / "submission.zip")
        def cancelled(signum, frame):
            raise SigningError("signing cancelled; inspect saved submission evidence before any retry")
        previous_handler = signal.signal(signal.SIGTERM, cancelled)
        try:
            invoke(["/usr/bin/security", "create-keychain", "-p", password, str(keychain)])
            invoke(["/usr/bin/security", "set-keychain-settings", "-lut", "1800", str(keychain)])
            invoke(["/usr/bin/security", "unlock-keychain", "-p", password, str(keychain)])
            invoke(["/usr/bin/security", "import", str(cert), "-P", certificate_password,
                    "-T", "/usr/bin/codesign", "-t", "cert", "-f", "pkcs12", "-k", str(keychain)])
            invoke(["/usr/bin/security", "set-key-partition-list", "-S", "apple-tool:,apple:",
                    "-s", "-k", password, str(keychain)])
            found = invoke(["/usr/bin/security", "find-identity", "-v", "-p", "codesigning", str(keychain)])
            if not re.search(rf'\b{identity["identity_sha1"]}\s+"Developer ID Application:', found, re.IGNORECASE):
                raise SigningError("approved Developer ID fingerprint not found in temporary keychain")
            invoke(argv["sign"])
            invoke(argv["verify_signature"])
            details = inspect_signature(binary, identity)
            details["signed_binary_sha256"] = release.digest(binary)
            submission_zip(binary, evidence / "submission.zip")
            archive_sha = release.digest(evidence / "submission.zip")
            # Save intent first. If submit itself times out, outcome is uncertain;
            # this workflow deliberately does not retry or resubmit automatically.
            pending = {**source, **details, "status": "submission-outcome-unknown", "submission_zip_sha256": archive_sha}
            release.dump_json(evidence / "submission.json", pending)
            identifier = submission_id(json_object(invoke(argv["submit"], timeout=300)))
            details["submission_id"] = identifier
            pending.update(status="In Progress", submission_id=identifier)
            release.dump_json(evidence / "submission.json", pending)
            wait = [identifier if value == "SUBMISSION_ID" else value for value in argv["wait"]]
            try:
                # notarytool can return nonzero for a terminal rejection. Parse
                # its JSON verdict and preserve the log without retrying upload.
                response = json_object(invoke(wait, timeout=1200, allow_failure=True))
                if response.get("id") != identifier or response.get("status") not in ("Accepted", "Invalid", "Rejected", "In Progress"):
                    raise SigningError("notarization returned an unexpected status or submission ID")
            except (SigningError, ValueError):
                pending["status"] = "status-check-unverified"
                release.dump_json(evidence / "submission.json", pending)
                raise
            pending["status"] = response["status"]
            release.dump_json(evidence / "submission.json", pending)
            if response["status"] == "In Progress":
                raise SigningError("notarization remains pending; inspect by saved ID before any retry")
            log_command = [identifier if value == "SUBMISSION_ID" else str(evidence / "notary-log.json")
                           if value == "NOTARY_LOG_PATH" else value for value in argv["log"]]
            invoke(log_command)
            verify_notary_log(json_object(release.regular_file(evidence / "notary-log.json")),
                              identifier, archive_sha, details["cdhash"], ctx["target"])
            invoke(argv["verify_notarization"])
            if release.digest(binary) != details["signed_binary_sha256"]:
                raise SigningError("signed binary changed during notarization")
            # Remove signing credentials before any program from the candidate is
            # executed. CLI smoke belongs to a fresh credential-free verify job.
            metadata = write_candidate(contents, binary, output / "bundle", ctx, details)
            result = {**source, **details, "status": "signed-and-notarized", "signing": identity,
                      "submission_zip_sha256": archive_sha, "signed_archive": metadata["archive"],
                      "signed_archive_sha256": metadata["sha256"], "publication": "forbidden",
                      "stapling": "not-applicable-standalone-cli", "fresh_machine_gatekeeper": "not-tested"}
            release.dump_json(evidence / "result.json", result)
            pending["status"] = "Accepted"
            release.dump_json(evidence / "submission.json", pending)
        finally:
            try:
                if keychain.exists():
                    invoke(["/usr/bin/security", "delete-keychain", str(keychain)])
            finally:
                signal.signal(signal.SIGTERM, previous_handler)
    return result


def verify_candidate(bundle: Path, evidence: Path, ctx: dict, identity: dict, *, native: bool) -> dict:
    """Cross-check recorded evidence; native mode independently rechecks Apple trust."""
    metadata = release.verify_bundle(bundle, ctx["version"], ctx["target"], execute=False)
    result = json_object(release.regular_file(evidence / "result.json"))
    expected = {**ctx, "status": "signed-and-notarized", "signing": identity,
                "signed_archive": metadata["archive"], "signed_archive_sha256": metadata["sha256"],
                "publication": "forbidden", "stapling": "not-applicable-standalone-cli",
                "fresh_machine_gatekeeper": "not-tested"}
    if any(result.get(key) != value for key, value in expected.items()):
        raise SigningError("signed candidate evidence does not match archive or expected identity/context")
    source = {**ctx, "unsigned_archive": result.get("unsigned_archive"), "unsigned_sha256": result.get("unsigned_sha256")}
    matching(source["unsigned_sha256"], r"[0-9a-f]{64}", "unsigned archive hash")
    if (source["unsigned_archive"] != metadata["archive"]
            or json_object(release.regular_file(evidence / "source.json")) != source):
        raise SigningError("source evidence does not match the recorded input identity")
    archive_sha = hashlib.sha256(release.regular_file(evidence / "submission.zip")).hexdigest()
    if archive_sha != result.get("submission_zip_sha256"):
        raise SigningError("notarization submission ZIP was changed")
    verify_notary_log(json_object(release.regular_file(evidence / "notary-log.json")),
                      result["submission_id"], archive_sha, result["cdhash"], ctx["target"])
    contents = contents_from_bundle(bundle, ctx)
    if hashlib.sha256(contents["loboot"]).hexdigest() != result.get("signed_binary_sha256"):
        raise SigningError("archived binary differs from signing evidence")
    with zipfile.ZipFile(evidence / "submission.zip") as archive:
        if archive.namelist() != ["loboot"] or archive.read("loboot") != contents["loboot"]:
            raise SigningError("submitted ZIP does not contain exactly the archived signed binary")
    if native:
        if sys.platform != "darwin" or any(os.environ.get(name) for name in SECRET_NAMES):
            raise SigningError("native verification requires credential-free macOS")
        release.require_native_target(ctx["target"])
        with tempfile.TemporaryDirectory(prefix="loboot-signing-check-") as temporary:
            binary = Path(temporary).resolve() / "loboot"
            binary.write_bytes(contents["loboot"])
            binary.chmod(0o700)
            argv = commands(identity, binary, Path("unused"), Path("unused"), Path("unused"))
            invoke(argv["verify_signature"])
            details = inspect_signature(binary, identity)
            if details["cdhash"] != result["cdhash"]:
                raise SigningError("archived signature CDHash differs from notarization evidence")
            invoke(argv["verify_notarization"])
            release.smoke(binary, ctx["version"], ctx["target"])
    return {**expected, "integrity": "passed", "native_trust_and_offline_smoke": "passed" if native else "not-run"}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("bind-input", "plan", "execute", "verify"))
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--provenance", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--evidence", type=Path)
    parser.add_argument("--native", action="store_true")
    for name in ("target", "version", "commit", "run-id"):
        parser.add_argument(f"--{name}", required=True)
    for name in ("team-id", "identity", "identifier", "key-id", "issuer"):
        parser.add_argument(f"--{name}")
    args = parser.parse_args(argv)
    try:
        ctx = context(args.target, args.version, args.commit, args.run_id)
        if args.command == "bind-input":
            if args.output is None:
                raise SigningError("bind-input requires --output")
            value = bind_input(args.bundle, ctx, args.output)
        else:
            identity = signing_identity(args.team_id, args.identity, args.identifier, args.key_id, args.issuer)
            if args.command == "verify":
                if args.evidence is None:
                    raise SigningError("verify requires --evidence")
                value = verify_candidate(args.bundle, args.evidence, ctx, identity, native=args.native)
            else:
                if args.provenance is None:
                    raise SigningError("plan/execute requires --provenance")
                if args.command == "plan":
                    value = plan(args.bundle, args.provenance, ctx, identity)
                else:
                    if args.output is None:
                        raise SigningError("execute requires --output")
                    value = execute(args.bundle, args.provenance, args.output, ctx, identity)
        print(json.dumps(value, sort_keys=True))
        return 0
    except (SigningError, release.ReleaseError, OSError, ValueError, TypeError, KeyError, tarfile.TarError, zipfile.BadZipFile) as error:
        # Never echo arbitrary errors from JSON/private input processing.
        message = str(error) if isinstance(error, (SigningError, release.ReleaseError)) else type(error).__name__
        print(f"macos-signing: {message}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
