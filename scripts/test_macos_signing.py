#!/usr/bin/env python3
"""Offline signing boundary tests. All Apple processes are explicit doubles."""
from contextlib import redirect_stderr, redirect_stdout
import base64
import hashlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import macos_signing as signing
import release


TARGET = "aarch64-apple-darwin"
COMMIT = "a" * 40
TEAM = "ABCDE12345"
IDENTITY = "B" * 40
IDENTIFIER = "io.github.WhiteKiwi.loboot"
ISSUER = "12345678-1234-1234-1234-123456789abc"
SUBMISSION = "23456789-1234-1234-1234-123456789abc"
CDHASH = "c" * 40


class SigningTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="loboot-signing-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.bundle = self.root / "unsigned"
        self.bundle.mkdir()
        self.ctx = signing.context(TARGET, "0.1.0", COMMIT, "123")
        self.identity = signing.signing_identity(TEAM, IDENTITY, IDENTIFIER, "KEY1234567", ISSUER)
        self.provenance = self.root / "provenance.json"
        header = bytearray(64)
        header[:4] = b"\xcf\xfa\xed\xfe"
        header[4:8] = (0x0100000C).to_bytes(4, "little")
        header[12:16] = (2).to_bytes(4, "little")
        self.payload = bytes(header) + b"fixture-not-executable"
        self.contents = {"loboot": self.payload, "README.md": release.package_readme("0.1.0", TARGET, None),
                         "LICENSE-STATUS.txt": b"license", "THIRD-PARTY-LICENSES.txt": b"notices",
                         "RUST-COPYRIGHT.html": b"<html>notices</html>"}
        self.write_bundle(self.bundle, self.contents)
        signing.bind_input(self.bundle, self.ctx, self.provenance)
        self.env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                    "GITHUB_REPOSITORY": signing.REPOSITORY, "GITHUB_EVENT_NAME": "workflow_dispatch",
                    "GITHUB_REF": "refs/heads/main", "GITHUB_SHA": COMMIT, "GITHUB_RUN_ID": "123",
                    "LOBOOT_SIGNING_ENABLED": "true", "LOBOOT_SIGNING_APPROVED": "true",
                    "MACOS_CERTIFICATE_BASE64": base64.b64encode(b"fake cert").decode(),
                    "MACOS_CERTIFICATE_PASSWORD": "not-a-real-secret", "MACOS_NOTARY_KEY": "-----BEGIN PRIVATE KEY-----\nfixture"}
        self.calls = []

    def write_bundle(self, directory, contents):
        stem = release.archive_stem("0.1.0", TARGET)
        archive = directory / f"{stem}.tar.gz"
        release.write_archive(archive, stem, contents)
        metadata = release.bundle_metadata("0.1.0", TARGET, archive)
        release.dump_json(directory / "manifest.json", metadata)
        (directory / "SHA256SUMS.txt").write_text(f'{metadata["sha256"]}  {archive.name}\n')

    def signature(self):
        return (f"Identifier={IDENTIFIER}\nTeamIdentifier={TEAM}\nCDHash={CDHASH}\n"
                "Authority=Developer ID Application: Fixture (ABCDE12345)\n"
                "Timestamp=Oct 14, 2026 at 1:00:00 AM\n"
                "CodeDirectory v=20500 size=200 flags=0x10000(runtime) hashes=1+2\n")

    def notary_log(self, archive_sha):
        return {"jobId": SUBMISSION, "status": "Accepted", "statusCode": 0, "sha256": archive_sha,
                "issues": None, "ticketContents": [{"path": "submission.zip/loboot", "arch": "arm64", "cdhash": CDHASH}]}

    def fake_invoke(self, argv, **kwargs):
        self.calls.append(argv)
        if argv[0] == "/usr/bin/security":
            if argv[1] == "create-keychain":
                Path(argv[-1]).write_text("fixture keychain")
            if argv[1] == "delete-keychain":
                Path(argv[-1]).unlink()
            if argv[1] == "find-identity":
                return f'1) {IDENTITY} "Developer ID Application: Fixture ({TEAM})"\n'
        if argv[0] == "/usr/bin/codesign":
            if "--sign" in argv:
                path = Path(argv[-1])
                path.write_bytes(path.read_bytes() + b"signed")
            if "--display" in argv:
                return self.signature()
        if argv[:3] == ["/usr/bin/xcrun", "notarytool", "submit"]:
            return json.dumps({"id": SUBMISSION})
        if argv[:3] == ["/usr/bin/xcrun", "notarytool", "wait"]:
            return json.dumps({"id": SUBMISSION, "status": "Accepted"})
        if argv[:3] == ["/usr/bin/xcrun", "notarytool", "log"]:
            log_path = Path(argv[-1])
            release.dump_json(log_path, self.notary_log(release.digest(log_path.parent / "submission.zip")))
        return ""

    def execute(self, fake=None, **kwargs):
        with patch.dict(os.environ, self.env, clear=True), patch.object(signing.sys, "platform", "darwin"), \
             patch.object(signing, "invoke", side_effect=fake or self.fake_invoke), \
             patch.object(signing, "inspect_signature", return_value=signing.signature_details(self.signature(), self.identity)), \
             patch.object(release, "smoke", side_effect=AssertionError("secret-bearing job cannot run candidate")), \
             patch.object(release, "check_version", side_effect=AssertionError("secret-bearing job cannot run candidate")):
            return signing.execute(self.bundle, self.provenance, self.root / "candidate", self.ctx, self.identity, **kwargs)

    def test_plan_is_pure_and_has_no_secret_values_or_stapling(self):
        with patch.dict(os.environ, self.env, clear=True), patch.object(signing, "invoke", side_effect=AssertionError("no tools")):
            plan = signing.plan(self.bundle, self.provenance, self.ctx, self.identity)
        self.assertEqual(plan["status"], "planned")
        self.assertEqual(plan["publication"], "forbidden")
        text = json.dumps(plan)
        self.assertNotIn(self.env["MACOS_CERTIFICATE_PASSWORD"], text)
        self.assertNotIn(self.env["MACOS_NOTARY_KEY"], text)
        self.assertNotIn("stapler", text)
        sign = plan["commands"]["sign"]
        self.assertIn("--timestamp", sign)
        self.assertIn("runtime", sign)
        self.assertIn(IDENTITY, sign)
        self.assertNotIn("--deep", sign)
        self.assertLess(sign.index("--sign"), sign.index("--options"))
        for name in ("verify_signature", "verify_notarization"):
            command = plan["commands"][name]
            self.assertTrue(command[command.index("-R") + 1].startswith("="))

    def test_context_refuses_invalid_target_commit_run_and_version(self):
        for values in (("x86_64-unknown-linux-gnu", "0.1.0", COMMIT, "123"),
                       (TARGET, "v0.1.0", COMMIT, "123"), (TARGET, "0.1.0", "main", "123"),
                       (TARGET, "0.1.0", COMMIT, "0"), (TARGET, "0.1.0", COMMIT, "123\n456")):
            with self.subTest(values=values), self.assertRaises(ValueError):
                signing.context(*values)

    def test_identity_refuses_ambiguous_identity_and_injection(self):
        good = [TEAM, IDENTITY, IDENTIFIER, "KEY1234567", ISSUER]
        for index, value in ((0, 'TEAM" and true'), (1, "Developer ID Application"), (2, 'id" or true'),
                             (3, "key\nvalue"), (4, "not-uuid")):
            bad = good.copy()
            bad[index] = value
            with self.subTest(index=index), self.assertRaises(ValueError):
                signing.signing_identity(*bad)

    def test_binding_refuses_replacement(self):
        with self.assertRaises(FileExistsError):
            signing.bind_input(self.bundle, self.ctx, self.provenance)

    def test_provenance_binds_every_context_field_and_hash(self):
        original = json.loads(self.provenance.read_text())
        for key in original:
            bad = {**original, key: "wrong"}
            release.dump_json(self.provenance, bad)
            with self.subTest(key=key), self.assertRaisesRegex(signing.SigningError, "provenance"):
                signing.verify_input(self.bundle, self.provenance, self.ctx)
        release.dump_json(self.provenance, {**original, "extra": "unexpected"})
        with self.assertRaises(signing.SigningError):
            signing.verify_input(self.bundle, self.provenance, self.ctx)

    def test_input_archive_rejects_changed_hash_extra_members_or_links(self):
        archive = next(self.bundle.glob("*.tar.gz"))
        archive.write_bytes(archive.read_bytes() + b"change")
        with self.assertRaises(release.ReleaseError):
            signing.verify_input(self.bundle, self.provenance, self.ctx)
        self.write_bundle(self.bundle, self.contents)
        (self.bundle / "unexpected.txt").write_text("no")
        with self.assertRaises(release.ReleaseError):
            signing.verify_input(self.bundle, self.provenance, self.ctx)
        (self.bundle / "unexpected.txt").unlink()
        self.provenance.unlink()
        self.provenance.symlink_to(self.bundle / "manifest.json")
        with self.assertRaises(release.ReleaseError):
            signing.verify_input(self.bundle, self.provenance, self.ctx)

    def test_macos_architecture_is_checked_before_signing(self):
        wrong = bytearray(self.payload)
        wrong[4:8] = (0x01000007).to_bytes(4, "little")
        self.write_bundle(self.bundle, {**self.contents, "loboot": bytes(wrong)})
        with patch.object(signing, "invoke") as invoke, self.assertRaises(release.ReleaseError):
            signing.execute(self.bundle, self.provenance, self.root / "candidate", self.ctx, self.identity)
        invoke.assert_not_called()

    def test_all_dispatch_guards_fail_closed(self):
        for key in self.env:
            with self.subTest(key=key), patch.object(signing.sys, "platform", "darwin"):
                bad = {**self.env, key: ""}
                with self.assertRaises(signing.SigningError):
                    signing.require_execution(self.ctx, bad)
        for key, value in (("GITHUB_EVENT_NAME", "pull_request"), ("GITHUB_REF", "refs/tags/v0.1.0"),
                           ("RUNNER_ENVIRONMENT", "self-hosted"), ("GITHUB_REPOSITORY", "fork/loboot")):
            with self.subTest(key=key), patch.object(signing.sys, "platform", "darwin"), self.assertRaises(signing.SigningError):
                signing.require_execution(self.ctx, {**self.env, key: value})

    def test_missing_credentials_produce_no_process_or_output(self):
        for key in signing.SECRET_NAMES:
            bad = {**self.env, key: ""}
            with patch.dict(os.environ, bad, clear=True), patch.object(signing.sys, "platform", "darwin"), \
                 patch.object(signing, "invoke") as invoke, self.assertRaisesRegex(signing.SigningError, "missing"):
                signing.execute(self.bundle, self.provenance, self.root / "candidate", self.ctx, self.identity)
            invoke.assert_not_called()
            self.assertFalse((self.root / "candidate").exists())

    def test_invalid_certificate_fails_before_side_effects(self):
        self.env["MACOS_CERTIFICATE_BASE64"] = "!invalid!"
        with self.assertRaisesRegex(signing.SigningError, "Base64"):
            self.execute()
        self.assertFalse(self.calls)
        self.assertFalse((self.root / "candidate").exists())

    def test_signature_requires_identity_runtime_timestamp_and_cdhash(self):
        self.assertEqual(signing.signature_details(self.signature(), self.identity)["cdhash"], CDHASH)
        for before, after in ((TEAM, "WRONG12345"), (IDENTIFIER, "wrong.id"), ("Timestamp=", "Time="),
                              ("runtime", "adhoc"), ("Developer ID Application:", "Apple Development:"), (CDHASH, "invalid")):
            with self.subTest(before=before), self.assertRaises(signing.SigningError):
                signing.signature_details(self.signature().replace(before, after), self.identity)
        with self.assertRaisesRegex(signing.SigningError, "ambiguous"):
            signing.signature_details(self.signature() + f"CDHash={CDHASH}\n", self.identity)

    def test_leaf_certificate_must_match_approved_fingerprint(self):
        def fake(argv, **kwargs):
            self.assertNotIn("--extract-certificates", argv)
            for argument in argv:
                if argument.startswith("--extract-certificates="):
                    Path(argument.split("=", 1)[1] + "0").write_bytes(b"wrong certificate")
            return self.signature()
        with patch.object(signing, "invoke", side_effect=fake), self.assertRaisesRegex(signing.SigningError, "leaf certificate"):
            signing.inspect_signature(self.root / "unused", self.identity)

    def test_log_rejects_nonaccepted_identity_hash_architecture_and_warnings(self):
        original = self.notary_log("d" * 64)
        signing.verify_notary_log(original, SUBMISSION, "d" * 64, CDHASH, TARGET)
        for key, value in (("jobId", ISSUER), ("status", "In Progress"), ("statusCode", 4000),
                           ("sha256", "e" * 64), ("ticketContents", []), ("issues", [{"severity": "warning"}])):
            with self.subTest(key=key), self.assertRaises(signing.SigningError):
                signing.verify_notary_log({**original, key: value}, SUBMISSION, "d" * 64, CDHASH, TARGET)
        for key, value in (("cdhash", "e" * 40), ("arch", "x86_64"), ("path", "archive/not-loboot")):
            ticket = {**original["ticketContents"][0], key: value}
            with self.subTest(key=key), self.assertRaises(signing.SigningError):
                signing.verify_notary_log({**original, "ticketContents": [ticket]}, SUBMISSION, "d" * 64, CDHASH, TARGET)

    def test_malformed_json_and_log_fields_fail_with_sanitized_errors(self):
        for payload in ('[]', 'null', '"string"', '123', '{"x": 1, "x": 2}', '{"x": NaN}'):
            with self.subTest(payload=payload), self.assertRaises(signing.SigningError):
                signing.json_object(payload)
        for log in ([], None, "string", 123):
            with self.subTest(log=log), self.assertRaises(signing.SigningError):
                signing.verify_notary_log(log, SUBMISSION, "d" * 64, CDHASH, TARGET)
        original = self.notary_log("d" * 64)
        for field in (None, 123, [], {}):
            log = {**original, "ticketContents": [{**original["ticketContents"][0], "path": field}]}
            with self.subTest(field=field), self.assertRaises(signing.SigningError):
                signing.verify_notary_log(log, SUBMISSION, "d" * 64, CDHASH, TARGET)
        with self.assertRaises(signing.SigningError):
            signing.verify_notary_log({**original, "statusCode": False}, SUBMISSION, "d" * 64, CDHASH, TARGET)

    def test_success_preserves_inventory_licenses_and_exact_signed_bytes(self):
        result = self.execute()
        output = self.root / "candidate"
        self.assertEqual(result["status"], "signed-and-notarized")
        self.assertEqual(result["fresh_machine_gatekeeper"], "not-tested")
        contents = signing.contents_from_bundle(output / "bundle", self.ctx)
        self.assertEqual(set(contents), set(release.ARCHIVE_FILES))
        self.assertEqual(contents["loboot"], self.payload + b"signed")
        for name in ("LICENSE-STATUS.txt", "THIRD-PARTY-LICENSES.txt", "RUST-COPYRIGHT.html"):
            self.assertEqual(contents[name], self.contents[name])
        self.assertNotIn(signing.UNSIGNED_NOTICE.encode(), contents["README.md"])
        self.assertIn(COMMIT.encode(), contents["README.md"])
        self.assertIn(b"not a published release", contents["README.md"])
        self.assertEqual(self.calls[-1][1], "delete-keychain")
        self.assertFalse(any("stapler" in part for call in self.calls for part in call))
        self.assertFalse(any(Path(call[0]).name == "loboot" for call in self.calls))
        verified = signing.verify_candidate(output / "bundle", output / "evidence", self.ctx, self.identity, native=False)
        self.assertEqual(verified["integrity"], "passed")
        self.assertEqual(verified["native_trust_and_offline_smoke"], "not-run")
        combined = b"".join(p.read_bytes() for p in output.rglob("*") if p.is_file())
        self.assertNotIn(self.env["MACOS_CERTIFICATE_PASSWORD"].encode(), combined)
        self.assertNotIn(self.env["MACOS_NOTARY_KEY"].encode(), combined)

    def test_native_verification_rechecks_trust_and_smokes_only_without_credentials(self):
        self.execute()
        output = self.root / "candidate"
        with patch.dict(os.environ, {}, clear=True), patch.object(signing.sys, "platform", "darwin"), \
             patch.object(release, "require_native_target") as native, patch.object(release, "smoke") as smoke, \
             patch.object(signing, "inspect_signature", return_value=signing.signature_details(self.signature(), self.identity)), \
             patch.object(signing, "invoke", side_effect=self.fake_invoke):
            result = signing.verify_candidate(output / "bundle", output / "evidence", self.ctx, self.identity, native=True)
        native.assert_called_once_with(TARGET)
        smoke.assert_called_once()
        self.assertEqual(result["native_trust_and_offline_smoke"], "passed")
        with patch.dict(os.environ, self.env, clear=True), patch.object(signing.sys, "platform", "darwin"), self.assertRaises(signing.SigningError):
            signing.verify_candidate(output / "bundle", output / "evidence", self.ctx, self.identity, native=True)

    def test_failure_always_cleans_keychain_and_never_builds_candidate(self):
        def fake(argv, **kwargs):
            if argv[0] == "/usr/bin/codesign" and "--sign" in argv:
                raise signing.SigningError("fixture signing failure")
            return self.fake_invoke(argv, **kwargs)
        with self.assertRaisesRegex(signing.SigningError, "fixture"):
            self.execute(fake)
        self.assertEqual(self.calls[-1][1], "delete-keychain")
        self.assertFalse((self.root / "candidate" / "bundle").exists())
        self.assertFalse(Path(self.calls[-1][-1]).exists())

    def test_wait_timeout_preserves_id_and_zip_without_resubmission(self):
        def fake(argv, **kwargs):
            if argv[:3] == ["/usr/bin/xcrun", "notarytool", "wait"]:
                raise signing.SigningError("timeout; inspect saved evidence")
            return self.fake_invoke(argv, **kwargs)
        with self.assertRaisesRegex(signing.SigningError, "timeout"):
            self.execute(fake)
        evidence = self.root / "candidate" / "evidence"
        self.assertEqual(json.loads((evidence / "submission.json").read_text())["submission_id"], SUBMISSION)
        self.assertTrue((evidence / "submission.zip").exists())
        self.assertEqual(sum(call[:3] == ["/usr/bin/xcrun", "notarytool", "submit"] for call in self.calls), 1)
        self.assertEqual(self.calls[-1][1], "delete-keychain")
        self.assertFalse((self.root / "candidate" / "bundle").exists())

    def test_terminal_rejection_saves_status_and_log_without_candidate(self):
        def fake(argv, **kwargs):
            if argv[:3] == ["/usr/bin/xcrun", "notarytool", "wait"]:
                self.assertTrue(kwargs["allow_failure"])
                return json.dumps({"id": SUBMISSION, "status": "Invalid"})
            result = self.fake_invoke(argv, **kwargs)
            if argv[:3] == ["/usr/bin/xcrun", "notarytool", "log"]:
                path = Path(argv[-1])
                release.dump_json(path, {**json.loads(path.read_text()), "status": "Invalid", "statusCode": 4000})
            return result
        with self.assertRaisesRegex(signing.SigningError, "not Accepted"):
            self.execute(fake)
        evidence = self.root / "candidate" / "evidence"
        self.assertEqual(json.loads((evidence / "submission.json").read_text())["status"], "Invalid")
        self.assertEqual(json.loads((evidence / "notary-log.json").read_text())["status"], "Invalid")
        self.assertFalse((evidence / "result.json").exists())
        self.assertFalse((self.root / "candidate" / "bundle").exists())

    def test_sigterm_cleanup_runs_without_claiming_success(self):
        def fake(argv, **kwargs):
            if argv[0] == "/usr/bin/codesign" and "--sign" in argv:
                signal.raise_signal(signal.SIGTERM)
            return self.fake_invoke(argv, **kwargs)
        previous = signal.getsignal(signal.SIGTERM)
        with self.assertRaisesRegex(signing.SigningError, "cancelled"):
            self.execute(fake)
        self.assertEqual(signal.getsignal(signal.SIGTERM), previous)
        self.assertEqual(self.calls[-1][1], "delete-keychain")
        self.assertFalse((self.root / "candidate" / "bundle").exists())

    def test_keyboard_interrupt_cleanup_runs_without_claiming_success(self):
        def fake(argv, **kwargs):
            if argv[0] == "/usr/bin/codesign" and "--sign" in argv:
                raise KeyboardInterrupt()
            return self.fake_invoke(argv, **kwargs)
        with self.assertRaises(KeyboardInterrupt):
            self.execute(fake)
        self.assertEqual(self.calls[-1][1], "delete-keychain")
        self.assertFalse((self.root / "candidate" / "bundle").exists())

    def test_submit_timeout_records_uncertain_outcome_and_no_false_success(self):
        def fake(argv, **kwargs):
            if argv[:3] == ["/usr/bin/xcrun", "notarytool", "submit"]:
                raise signing.SigningError("timeout")
            return self.fake_invoke(argv, **kwargs)
        with self.assertRaises(signing.SigningError):
            self.execute(fake)
        evidence = self.root / "candidate" / "evidence"
        self.assertEqual(json.loads((evidence / "submission.json").read_text())["status"], "submission-outcome-unknown")
        self.assertFalse((evidence / "result.json").exists())

    def test_tampered_result_archive_and_zip_fail_closed(self):
        self.execute()
        output = self.root / "candidate"
        result_path = output / "evidence" / "result.json"
        result = json.loads(result_path.read_text())
        for key in ("commit", "run_id", "target", "signed_archive_sha256", "signed_binary_sha256", "cdhash"):
            release.dump_json(result_path, {**result, key: "invalid"})
            with self.subTest(key=key), self.assertRaises(ValueError):
                signing.verify_candidate(output / "bundle", output / "evidence", self.ctx, self.identity, native=False)
        release.dump_json(result_path, result)
        (output / "evidence" / "submission.zip").write_bytes(b"corrupt")
        with self.assertRaisesRegex(signing.SigningError, "ZIP"):
            signing.verify_candidate(output / "bundle", output / "evidence", self.ctx, self.identity, native=False)

    def test_post_notarization_binary_change_is_rejected(self):
        def fake(argv, **kwargs):
            result = self.fake_invoke(argv, **kwargs)
            if "--check-notarization" in argv:
                Path(argv[-1]).write_bytes(self.payload + b"altered")
            return result
        with self.assertRaisesRegex(signing.SigningError, "changed"):
            self.execute(fake)
        self.assertFalse((self.root / "candidate" / "bundle").exists())

    def test_invoker_scrubs_child_environment_and_never_echoes_secrets(self):
        process = subprocess.CompletedProcess([], 1, "secret stdout", "secret stderr")
        with patch.dict(os.environ, {**self.env, "GH_TOKEN": "secret"}, clear=True), \
             patch.object(subprocess, "run", return_value=process) as run, self.assertRaises(signing.SigningError) as caught:
            signing.invoke(["/usr/bin/security", "import", "password"])
        self.assertNotIn("secret", str(caught.exception))
        self.assertNotIn("password", str(caught.exception))
        self.assertEqual(set(run.call_args.kwargs["env"]), {"PATH", "LC_ALL"})

    def test_invoker_timeout_message_does_not_echo_command(self):
        with patch.object(subprocess, "run", side_effect=subprocess.TimeoutExpired(["secret"], 60)), \
             self.assertRaises(signing.SigningError) as caught:
            signing.invoke(["/usr/bin/security", "secret"])
        self.assertNotIn("secret", str(caught.exception))

    def test_cli_errors_are_json_safe_and_do_not_start_apple_tools(self):
        args = ["plan", "--bundle", str(self.bundle), "--target", TARGET, "--version", "0.1.0",
                "--commit", COMMIT, "--run-id", "123"]
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr), patch.object(signing, "invoke") as invoke:
            self.assertEqual(signing.main(args), 1)
        invoke.assert_not_called()
        self.assertEqual(stdout.getvalue(), "")
        self.assertTrue(stderr.getvalue().startswith("macos-signing:"))


class WorkflowBoundaryTests(unittest.TestCase):
    def test_workflow_keeps_secrets_outside_pr_build_and_verify(self):
        root = Path(__file__).resolve().parent.parent
        workflow = (root / ".github/workflows/macos-signing.yml").read_text()
        gate, remainder = workflow.split("  build:", 1)
        build, remainder = remainder.split("  sign:", 1)
        sign, verify = remainder.split("  verify:", 1)
        self.assertNotIn("secrets.", gate + build + verify)
        self.assertNotIn("pull_request:", workflow)
        self.assertNotIn("pull_request_target:", workflow)
        self.assertNotIn("workflow_call:", workflow)
        self.assertNotIn("workflow_run:", workflow)
        self.assertIn("environment: macos-signing", sign)
        self.assertIn("needs.gate.outputs.commit == github.sha", sign)
        self.assertIn("inputs.authorize_signing_upload", sign)
        self.assertIn("vars.MACOS_SIGNING_ENABLED == 'true'", sign)
        self.assertIn("github.ref == 'refs/heads/main'", sign)
        self.assertNotIn("cargo ", sign)
        self.assertNotIn("scripts/release.py check", sign)
        self.assertNotIn("scripts/release.py build", sign)
        for forbidden in ("contents: write", "gh release", "git push", "secrets: inherit", "loboot-dist-", "run-id:"):
            self.assertNotIn(forbidden, workflow)
        readiness = (root / ".github/workflows/release-readiness.yml").read_text()
        publisher = (root / ".github/workflows/release.yml").read_text()
        self.assertNotIn("secrets.", readiness)
        self.assertNotIn("macos-signed-candidate", publisher)
        self.assertNotIn("macos-signing.yml", publisher)


if __name__ == "__main__":
    unittest.main()
