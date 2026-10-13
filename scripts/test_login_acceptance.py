#!/usr/bin/env python3
"""Synthetic evidence tests only: no manager calls and no real session transitions."""
from contextlib import redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"{name}.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


acceptance = load("login_acceptance")
fixture = load("login_fixture")
RUN = "0123456789abcdef0123456789abcdef"


class AcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.sequence = 0

    def save(self, value, raw=False):
        self.sequence += 1
        path = self.root / f"artifact-{self.sequence}"
        data = value if raw else json.dumps(value).encode()
        path.write_bytes(data)
        return {"path": path.name, "sha256": hashlib.sha256(data).hexdigest()}

    def report(self, system="Linux"):
        """Deliberately synthetic: validator tests are never acceptance evidence."""
        result = acceptance.template(system, RUN)
        result.update(status="passed", reason=None, fixture_directory="/home/test/fixture",
                      python="/usr/bin/python3", fixture_source=self.save(b"synthetic fixture", raw=True))
        result["host"].update(os_version="SYNTHETIC TEST OS", architecture="test-only", uid=123,
                              home="/home/test", boot_identity="synthetic-boot", linger=False if system == "Linux" else "not_applicable")
        result["build"].update(revision="a" * 40, binary_sha256="b" * 64, version="test-only")
        result["authorization"] = {"isolated_account": True, "native_changes": True,
                                   "two_logout_login_cycles": True, "reference": "synthetic test only"}
        result["command_transcript"] = self.save(b"synthetic transcript", raw=True)
        for mode, value in result["fixtures"].items():
            manifest = {"id": value["id"], "mode": mode, "restart": "never" if mode == "once" else "always",
                        "cwd": "/home/test/fixture", "executable": "/usr/bin/python3",
                        "args": ["/home/test/fixture/fixture.py", "--run-id", RUN, "--mode", mode,
                                 "--log", f"/home/test/fixture/{mode}.jsonl"]}
            value["show"] = self.save({"schema_version": "loboot.cli/v1", "ok": True,
                                       "data": {"service": {"manifest": manifest}}})
        for stage, minute in zip(acceptance.STAGES, (0, 3, 4, 7, 8)):
            observation = result["observations"][stage]
            observation["at"] = f"2026-10-13T12:{minute:02d}:00Z"
            for mode, value in observation["jobs"].items():
                value["status"] = self.save({"schema_version": "loboot.cli/v1", "ok": True, "data": {
                    "id": result["fixtures"][mode]["id"], "installed": True,
                    "native": {"enabled": stage in ("armed", "activated"),
                               "running": stage == "activated" and mode == "service",
                               "loaded": stage == "activated"}}})
                event = {"schema_version": "loboot.login-event/v1", "run_id": RUN, "mode": mode,
                         "uid": 123, "pid": 4321, "at": "2026-10-13T12:02:00Z"}
                value["events"] = self.save(b"" if stage == "armed" else (json.dumps(event) + "\n").encode(), raw=True)
        for name, minute, first, second in (("enabled", 1, "session-1", "session-2"),
                                             ("disabled", 5, "session-2", "session-3")):
            result["boundaries"][name] = {
                "real_logout_login": True, "no_manual_activation": True, "session_absent_observed": True,
                "before_identity": first, "after_identity": second, "boot_identity": "synthetic-boot",
                "logout_at": f"2026-10-13T12:{minute:02d}:00Z", "absent_at": f"2026-10-13T12:{minute:02d}:20Z",
                "login_at": f"2026-10-13T12:{minute+1:02d}:00Z", "observer": "synthetic test observer",
                "proof": self.save(b"synthetic boundary proof", raw=True)}
        ids = [result["fixtures"][mode]["id"] for mode in acceptance.MODES]
        cleanup = result["cleanup"]
        cleanup.update(at="2026-10-13T12:09:00Z", only_test_owned=True, jobs_absent=True,
                       processes_exited=True, removed_ids=ids, proof=self.save(b"synthetic cleanup", raw=True),
                       retained_disabled_labels=[f"com.loboot.{id}" for id in ids] if system == "Darwin" else [])
        for scope in ("native", "state"):
            cleanup[f"{scope}_before"] = self.save({"exists": False, "entries": {}})
            cleanup[f"{scope}_after"] = self.save({"exists": False, "entries": {}})
        return result

    def validate(self, report):
        return acceptance.validate(report, self.root)

    def change_artifact(self, reference, change):
        data = json.loads((self.root / reference["path"]).read_bytes())
        change(data)
        return self.save(data)

    def test_both_platform_reports_are_consistent_but_not_independent_proof(self):
        for system in ("Linux", "Darwin"):
            result = self.validate(self.report(system))
            self.assertTrue(result["consistent"])
            self.assertFalse(result["independently_verified"])

    def test_template_is_never_a_pass(self):
        result = self.validate(acceptance.template("Linux", RUN))
        self.assertEqual(result["reported_status"], "not_run")
        self.assertFalse(result["consistent"])

    def test_nonpassing_reports_require_reasons(self):
        for status in ("blocked", "failed", "not_run"):
            report = acceptance.template("Darwin", RUN)
            report["status"] = status
            self.assertFalse(self.validate(report)["consistent"])
            report["reason"] = ""
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_hosted_smoke_schema_rejected(self):
        report = self.report()
        report["schema_version"] = "loboot.native-smoke/v1"
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_missing_platform_metadata_and_root_refused(self):
        for field, value in (("uid", 0), ("uid", True), ("os_version", None), ("architecture", ""),
                             ("home", "relative"), ("boot_identity", None), ("linger", True)):
            with self.subTest(field=field), self.assertRaises(acceptance.EvidenceError):
                report = self.report()
                report["host"][field] = value
                self.validate(report)

    def test_authorization_fields_are_explicit_booleans(self):
        for field in ("isolated_account", "native_changes", "two_logout_login_cycles"):
            for value in (False, "true", 1):
                with self.subTest(field=field, value=value), self.assertRaises(acceptance.EvidenceError):
                    report = self.report()
                    report["authorization"][field] = value
                    self.validate(report)

    def test_refuses_same_identity_fake_boundary_and_reboot(self):
        for field, value in (("after_identity", "session-1"), ("real_logout_login", False),
                             ("no_manual_activation", False), ("session_absent_observed", False),
                             ("boot_identity", "rebooted"), ("observer", "")):
            with self.subTest(field=field), self.assertRaises(acceptance.EvidenceError):
                report = self.report()
                report["boundaries"]["enabled"][field] = value
                self.validate(report)

    def test_manager_lifetime_must_be_continuous_and_then_turn_over(self):
        for key, value in (("before_identity", "another-manager"), ("after_identity", "session-1")):
            with self.assertRaises(acceptance.EvidenceError):
                report = self.report()
                report["boundaries"]["disabled"][key] = value
                self.validate(report)

    def test_invalid_boundary_order_and_naive_timestamps(self):
        for key, value in (("logout_at", "2026-10-13T11:00:00Z"),
                           ("absent_at", "2026-10-13T12:04:00Z"), ("login_at", "2026-10-13T12:02:00")):
            with self.subTest(key=key), self.assertRaises(acceptance.EvidenceError):
                report = self.report()
                report["boundaries"]["enabled"][key] = value
                self.validate(report)

    def test_observation_window_cannot_be_shortened(self):
        report = self.report()
        report["observations"]["observed"]["at"] = "2026-10-13T12:07:29Z"
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_unknown_enabled_or_running_does_not_count_as_false(self):
        for field in ("enabled", "running"):
            report = self.report()
            job = report["observations"]["disabled"]["jobs"]["once"]
            job["status"] = self.change_artifact(job["status"], lambda x: x["data"]["native"].update({field: None}))
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_macos_armed_job_cannot_be_loaded(self):
        report = self.report("Darwin")
        job = report["observations"]["armed"]["jobs"]["once"]
        job["status"] = self.change_artifact(job["status"], lambda x: x["data"]["native"].update(loaded=True))
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_status_wrong_id_or_failed_envelope_refused(self):
        for edit in (lambda x: x["data"].update(id="foreign"), lambda x: x.update(ok=False)):
            report = self.report()
            job = report["observations"]["activated"]["jobs"]["once"]
            job["status"] = self.change_artifact(job["status"], edit)
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_duplicate_incomplete_and_wrong_run_events_refused(self):
        for mutation in (lambda x: x + x, lambda x: x.rstrip(), lambda x: x.replace(RUN.encode(), b"f" * 32)):
            report = self.report()
            job = report["observations"]["observed"]["jobs"]["once"]
            job["events"] = self.save(mutation((self.root / job["events"]["path"]).read_bytes()), raw=True)
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_replaced_event_history_refused(self):
        report = self.report()
        job = report["observations"]["observed"]["jobs"]["service"]
        raw = (self.root / job["events"]["path"]).read_bytes().replace(b'4321', b'1234')
        job["events"] = self.save(raw, raw=True)
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_prelogin_invocation_refused(self):
        report = self.report()
        for stage in acceptance.STAGES[1:]:
            job = report["observations"][stage]["jobs"]["once"]
            raw = (self.root / job["events"]["path"]).read_bytes().replace(b"12:02:00", b"12:00:30")
            job["events"] = self.save(raw, raw=True)
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_manifest_must_match_scope_and_fixture_command(self):
        for change in ({"mode": "service"}, {"restart": "always"}, {"id": "foreign"}, {"args": []}):
            report = self.report()
            job = report["fixtures"]["once"]
            job["show"] = self.change_artifact(job["show"], lambda x: x["data"]["service"]["manifest"].update(change))
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_cleanup_cannot_touch_foreign_id_or_hide_errors(self):
        for field, value in (("removed_ids", ["foreign"]), ("errors", ["not removed"]),
                             ("only_test_owned", False), ("jobs_absent", False), ("processes_exited", False),
                             ("retained_disabled_labels", ["foreign-label"])):
            report = self.report()
            report["cleanup"][field] = value
            with self.subTest(field=field), self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_cleanup_must_be_after_observation(self):
        report = self.report()
        report["cleanup"]["at"] = "2026-10-13T12:07:00Z"
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_inventory_changes_refused(self):
        for scope in ("native", "state"):
            report = self.report()
            report["cleanup"][f"{scope}_after"] = self.save({"exists": True, "entries": {}})
            with self.assertRaises(acceptance.EvidenceError):
                self.validate(report)

    def test_artifact_hash_tampering_refused(self):
        report = self.report()
        (self.root / report["command_transcript"]["path"]).write_text("changed")
        with self.assertRaises(acceptance.EvidenceError):
            self.validate(report)

    def test_artifact_traversal_absolute_and_symlink_refused(self):
        reference = self.save(b"proof", raw=True)
        (self.root / "link").symlink_to(self.root / reference["path"])
        for path in ("../escape", str(self.root / reference["path"]), "link"):
            with self.subTest(path=path), self.assertRaises((acceptance.EvidenceError, OSError)):
                acceptance.artifact(self.root, dict(reference, path=path))

    def test_duplicate_json_keys_refused(self):
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.read_json('{"status":"not_run","status":"passed"}')

    def test_template_and_validation_cli_do_not_launch_processes(self):
        with patch("subprocess.run") as run, redirect_stdout(io.StringIO()):
            self.assertEqual(acceptance.main(["template", "--system", "Linux", "--run-id", RUN]), 0)
            path = self.root / "report.json"
            path.write_text(json.dumps(acceptance.template("Linux", RUN)))
            self.assertEqual(acceptance.main(["validate", str(path)]), 77)
            path.write_text(json.dumps(self.report()))
            self.assertEqual(acceptance.main(["validate", str(path)]), 0)
            path.write_text("{}")
            self.assertEqual(acceptance.main(["validate", str(path)]), 1)
        run.assert_not_called()

    def test_bounded_read_refuses_growth_and_special_or_hardlinked_files(self):
        path = self.root / "bounded"
        path.write_bytes(b"12345")
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.read_regular(path, limit=4)
        real_fstat = os.fstat
        def stale_size(fd):
            values = list(real_fstat(fd))
            values[6] = 0
            return os.stat_result(values)
        with patch.object(acceptance.os, "fstat", side_effect=stale_size), self.assertRaises(acceptance.EvidenceError):
            acceptance.read_regular(path, limit=4)
        os.link(path, self.root / "alias")
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.read_regular(path)
        fifo = self.root / "pipe"
        os.mkfifo(fifo)
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.read_regular(fifo)
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.read_regular(self.root)

    def test_malformed_report_and_event_types_produce_json_errors(self):
        report_path = self.root / "report.json"
        values = [{}, [], {"schema_version": acceptance.SCHEMA, "provenance": "manual-isolated-account", "status": "passed"}]
        report = self.report()
        report["observations"]["activated"]["jobs"]["once"]["events"] = self.save(b"null\n", raw=True)
        values.append(report)
        for value in values:
            report_path.write_text(json.dumps(value))
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(acceptance.main(["validate", str(report_path)]), 1)
            self.assertFalse(json.loads(output.getvalue())["consistent"])
        with redirect_stdout(io.StringIO()):
            self.assertEqual(acceptance.main(["validate", str(self.root)]), 1)

    def test_nul_artifact_path_returns_json_error(self):
        report = self.report()
        report["command_transcript"]["path"] = "bad\0path"
        path = self.root / "report.json"
        path.write_text(json.dumps(report))
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(acceptance.main(["validate", str(path)]), 1)
        self.assertFalse(json.loads(output.getvalue())["consistent"])

    def test_inventory_refuses_large_hardlinked_and_special_files(self):
        directory = self.root / "inventory-limits"
        directory.mkdir()
        large = directory / "large"
        with large.open("wb") as target:
            target.truncate(4 * 1024 * 1024 + 1)
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.inventory(directory)
        large.unlink()
        source = self.root / "original"
        source.write_text("data")
        os.link(source, directory / "hardlink")
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.inventory(directory)
        (directory / "hardlink").unlink()
        os.mkfifo(directory / "fifo")
        with self.assertRaises(acceptance.EvidenceError):
            acceptance.inventory(directory)

    def test_inventory_tracks_missing_empty_regular_and_symlinks(self):
        path = self.root / "inventory"
        self.assertEqual(acceptance.inventory(path), {"exists": False, "entries": {}})
        path.mkdir()
        (path / "file").write_text("abc")
        (path / "link").symlink_to("/not/read")
        result = acceptance.inventory(path)
        self.assertTrue(result["exists"])
        self.assertEqual(result["entries"]["link"], {"symlink": "/not/read"})
        self.assertEqual(result["entries"]["file"], {"sha256": hashlib.sha256(b"abc").hexdigest()})
        (self.root / "alias").symlink_to(path)
        with self.assertRaises((acceptance.EvidenceError, OSError)):
            acceptance.inventory(self.root / "alias")


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.log = self.root / "fixture.jsonl"
        self.log.touch(mode=0o600)

    def test_recorder_appends_honest_invocations_without_native_calls(self):
        with patch("subprocess.run") as run:
            fixture.record(self.log, RUN, "once")
            fixture.record(self.log, RUN, "once")
        events = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(len(events), 2)
        self.assertTrue(all(event["uid"] == os.getuid() and event["pid"] == os.getpid() for event in events))
        run.assert_not_called()

    def test_does_not_create_missing_log(self):
        path = self.root / "missing"
        with self.assertRaises(OSError):
            fixture.record(path, RUN, "once")
        self.assertFalse(path.exists())

    def test_refuses_symlink_log_or_parent(self):
        for target in (self.root / "link", self.root / "parent" / self.log.name):
            if target.name == "link":
                target.symlink_to(self.log)
            else:
                target.parent.symlink_to(self.root)
            with self.assertRaises(OSError):
                fixture.record(target, RUN, "once")
        self.assertEqual(self.log.read_bytes(), b"")

    def test_refuses_hardlink_public_log_and_public_parent(self):
        os.link(self.log, self.root / "hardlink")
        with self.assertRaises(ValueError):
            fixture.record(self.log, RUN, "once")
        (self.root / "hardlink").unlink()
        self.log.chmod(0o644)
        with self.assertRaises(ValueError):
            fixture.record(self.log, RUN, "once")
        self.log.chmod(0o600)
        self.root.chmod(0o755)
        with self.assertRaises(ValueError):
            fixture.record(self.log, RUN, "once")

    def test_refuses_fifo_without_blocking(self):
        fifo = self.root / "fifo"
        os.mkfifo(fifo, mode=0o600)
        with self.assertRaises(OSError):
            fixture.record(fifo, RUN, "once")

    def test_refuses_malformed_identity_and_paths(self):
        for path, run, mode in ((self.log, "bad", "once"), (self.log, RUN, "wrong"),
                                (Path("relative"), RUN, "once"), (self.root / ".." / "log", RUN, "once")):
            with self.assertRaises(ValueError):
                fixture.record(path, run, mode)


if __name__ == "__main__":
    unittest.main()
