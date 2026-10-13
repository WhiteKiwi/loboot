#!/usr/bin/env python3
"""Offline tests of hosted-native smoke guards, assertions and cleanup.

Every manager/CLI action is mocked. This suite never registers a native job.
"""
from __future__ import annotations

import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("native_smoke", Path(__file__).with_name("native_smoke.py"))
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


class FakeSmoke(smoke.NativeSmoke):
    """In-memory manager double plus isolated owned file fixtures."""
    def __init__(self, root, system="Linux"):
        home = root / "home"
        home.mkdir()
        super().__init__(root / "loboot", home, root, system, {})
        self.jobs = {}
        self.calls = []
        self.available = True
        self.break_action = None

    @staticmethod
    def pid_exists(pid):
        return False

    def stable_count(self, id, count, seconds=3):
        smoke.require(len(self.events(id)) == count, "wrong invocation count")

    def command(self, args, env=None):
        self.calls.append(args)
        if args[0] == "doctor":
            return self.envelope({"state": {"ok": True}, "session": {"checked": True, "available": self.available}})
        if env and "missing-bus" in env.get("DBUS_SESSION_BUS_ADDRESS", ""):
            return self.error("session_unavailable")
        if self.break_action == args[0]:
            self.break_action = None
            return self.error("injected_failure")
        action, id = args
        if action in ("add", "update"):
            fields = dict(line.split(" = ", 1) for line in Path(id).read_text().splitlines())
            manifest = {key: json.loads(value) for key, value in fields.items()}
            id = manifest["id"]
            if id not in self.jobs:
                self.jobs[id] = {"enabled": False, "loaded": self.system == "Linux", "running": False, "raw": {}}
            self.jobs[id]["manifest"] = manifest
            self.state_dir.mkdir(parents=True, exist_ok=True)
            self.native_dir.mkdir(parents=True, exist_ok=True)
            (self.state_dir / ".lock").touch()
            (self.state_dir / f"{id}.toml").write_text(json.dumps(manifest))
            (self.native_dir / self.native_name(id)).write_text(json.dumps(manifest))
            return self.envelope({})
        if id not in self.jobs:
            return self.envelope({}) if action == "remove" else self.error("not_managed")
        job = self.jobs[id]
        if action == "show":
            manifest = json.loads((self.state_dir / f"{id}.toml").read_text())
            return self.envelope({"service": {"manifest": manifest}})
        if action == "status":
            return self.envelope({"native": job.copy()})
        if action == "enable":
            job["enabled"] = True
            if self.system == "Linux":
                directory = self.native_dir / "default.target.wants"
                directory.mkdir(exist_ok=True)
                link = directory / self.native_name(id)
                if not link.is_symlink():
                    link.symlink_to(self.native_dir / self.native_name(id))
            return self.envelope({})
        if action == "disable":
            if self.system == "Darwin" and job["loaded"]:
                return self.error("stop_first")
            job["enabled"] = False
            (self.native_dir / "default.target.wants" / self.native_name(id)).unlink(missing_ok=True)
            return self.envelope({})
        if action == "stop":
            job["running"] = False
            if self.system == "Darwin":
                job["loaded"] = False
            return self.envelope({})
        if action in ("start", "restart"):
            if self.system == "Darwin" and not job["enabled"]:
                return self.error("disabled_service")
            if action == "restart" or not job["running"]:
                kind, marker = job["manifest"]["args"][-2:]
                path = self.sandbox / f"{id}.jsonl"
                with path.open("a") as file:
                    file.write(json.dumps({"pid": 987654, "kind": kind, "marker": marker}) + "\n")
                job["running"] = kind == "service"
                job["loaded"] = True
                if kind == "fail":
                    job["raw"] = {"ExecMainStatus": "23", "last exit code": "23", "ActiveState": "failed"}
                    if self.system == "Linux":
                        return self.error("partial_failure")
            return self.envelope({})
        if action == "remove":
            smoke.require(not job["enabled"] and not job["running"], "unsafe removal")
            (self.state_dir / f"{id}.toml").unlink()
            (self.native_dir / self.native_name(id)).unlink()
            del self.jobs[id]
            return self.envelope({})
        raise AssertionError(args)

    def native_name(self, id):
        return f"loboot-{id}.service" if self.system == "Linux" else f"com.loboot.{id}.plist"

    def capture_failure_diagnostics(self):
        self.evidence["fake_failure_diagnostics"] = True

    def native_absent(self, id):
        smoke.require(id not in self.jobs, "fixture still loaded")

    @staticmethod
    def envelope(data):
        return {"schema_version": "loboot.cli/v1", "ok": True, "data": data}

    @staticmethod
    def error(code):
        return {"schema_version": "loboot.cli/v1", "ok": False, "error": {"code": code}}


class NativeSmokeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="loboot-smoke-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "Linux",
                    "HOME": str(self.root), "RUNNER_TEMP": str(self.root)}

    def guard(self, allow=True, env=None, system="Linux", uid=12345, home=None):
        return smoke.hosted_context(allow, env or self.env, system, uid, home or str(self.root))

    def test_opt_in_required_before_any_commands_or_files(self):
        with patch.object(smoke.subprocess, "run") as run, redirect_stdout(io.StringIO()):
            self.assertEqual(smoke.main(["--binary", "/does/not/exist"]), 2)
        run.assert_not_called()
        self.assertEqual(list(self.root.iterdir()), [])

    def test_self_hosted_and_non_actions_refused(self):
        for key, value in (("GITHUB_ACTIONS", "false"), ("RUNNER_ENVIRONMENT", "self-hosted")):
            with self.subTest(key=key), self.assertRaises(smoke.UnsafeInvocation):
                self.guard(env=dict(self.env, **{key: value}))

    def test_root_refused(self):
        with self.assertRaises(smoke.UnsafeInvocation):
            self.guard(uid=0)

    def test_platform_mismatch_refused(self):
        for system in ("Windows", "Darwin"):
            with self.subTest(system=system), self.assertRaises(smoke.UnsafeInvocation):
                self.guard(system=system)

    def test_nonabsolute_directories_refused(self):
        for key in ("HOME", "RUNNER_TEMP"):
            with self.subTest(key=key), self.assertRaises(smoke.UnsafeInvocation):
                self.guard(env=dict(self.env, **{key: "relative"}))

    def test_foreign_home_refused(self):
        other = self.root / "other"
        other.mkdir()
        with self.assertRaises(smoke.UnsafeInvocation):
            self.guard(home=str(other))

    def test_foreign_owned_runner_directory_refused(self):
        with self.assertRaises(smoke.UnsafeInvocation):
            self.guard(uid=os.getuid() + 1)

    def test_missing_bus_is_not_started(self):
        with patch.object(smoke.Path, "is_dir", return_value=False), patch.object(smoke.subprocess, "run") as run:
            self.assertEqual(smoke.existing_user_bus({}, 12345), {})
        run.assert_not_called()

    def test_existing_explicit_bus_is_preserved(self):
        env = {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/known/bus"}
        self.assertEqual(smoke.existing_user_bus(env, 12345), env)

    def test_snapshot_does_not_follow_links(self):
        native = self.root / "native"
        native.mkdir()
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "private").write_text("not followed")
        (native / "linked").symlink_to(outside, target_is_directory=True)
        self.assertEqual(smoke.snapshot(native), {"linked": ["symlink", str(outside)]})

    def test_symlinked_native_parent_refused(self):
        home = self.root / "home"
        home.mkdir()
        (home / ".config").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(smoke.SmokeError):
            smoke.absent_parents([home / ".config/systemd/user"], home)

    def test_existing_state_refused_without_manager_calls(self):
        fake = FakeSmoke(self.root)
        fake.state_dir.mkdir(parents=True)
        with self.assertRaises(smoke.SmokeError):
            fake.run()
        self.assertEqual(fake.calls, [])

    def test_unavailable_session_skips_without_mutating(self):
        fake = FakeSmoke(self.root)
        fake.available = False
        self.assertEqual(fake.run(), 77)
        self.assertEqual(fake.evidence["status"], "skipped")
        self.assertFalse(fake.evidence["login_activation_verified"])
        self.assertEqual(fake.calls, [["doctor"]])
        self.assertFalse(fake.state_dir.exists())
        self.assertIsNone(fake.sandbox)

    def test_full_linux_lifecycle_and_cleanup_offline_double(self):
        fake = FakeSmoke(self.root)
        with patch.object(smoke.subprocess, "run", return_value=SimpleNamespace(returncode=0, stdout="", stderr="")) as run:
            self.assertEqual(fake.run(), 0, fake.evidence)
        self.assertEqual(fake.jobs, {})
        self.assertFalse(fake.state_dir.exists())
        self.assertFalse(fake.native_dir.exists())
        self.assertFalse(fake.sandbox.exists())
        self.assertFalse(fake.evidence["login_activation_verified"])
        self.assertTrue(any("missing-session" in item for item in fake.evidence["checks"]))
        self.assertEqual(len(run.call_args_list), 1)
        self.assertIn("reset-failed", run.call_args.args[0])
        self.assertEqual(run.call_args.args[0][-1], f"loboot-{fake.ids[-1]}.service")

    def test_full_macos_lifecycle_and_cleanup_offline_double(self):
        fake = FakeSmoke(self.root, "Darwin")
        with patch.object(smoke.subprocess, "run") as run:
            self.assertEqual(fake.run(), 0, fake.evidence)
        run.assert_not_called()
        self.assertEqual(fake.jobs, {})
        self.assertEqual(len(fake.evidence["disabled_override_labels"]), 3)
        self.assertTrue(any("loaded-disable" in item for item in fake.evidence["checks"]))
        self.assertFalse(fake.native_dir.exists())

    def test_failure_still_removes_only_owned_resources(self):
        fake = FakeSmoke(self.root)
        fake.native_dir.mkdir(parents=True)
        foreign = fake.native_dir / "unrelated.service"
        foreign.write_text("original bytes")
        fake.break_action = "enable"
        self.assertEqual(fake.run(), 1)
        self.assertEqual(fake.jobs, {})
        self.assertEqual(foreign.read_text(), "original bytes")
        self.assertEqual(fake.evidence["cleanup_errors"], [])
        self.assertFalse(fake.sandbox.exists())

    def test_cleanup_drift_refuses_fallback_mutation(self):
        fake = FakeSmoke(self.root)
        self.assertTrue(fake.preflight())
        fake.sandbox = self.root / "sandbox"
        fake.sandbox.mkdir()
        id = fake.add("once")
        (fake.state_dir / f"{id}.toml").write_text('{"foreign": true}')
        before = len(fake.calls)
        fake.cleanup()
        self.assertEqual(fake.calls[before:], [["show", id]])
        self.assertTrue(fake.evidence["cleanup_errors"])
        self.assertTrue(fake.sandbox.exists())

    def test_update_failure_can_cleanup_previous_owned_manifest(self):
        fake = FakeSmoke(self.root)
        fake.break_action = "update"
        self.assertEqual(fake.run(), 1)
        self.assertEqual(fake.jobs, {})
        self.assertEqual(fake.evidence["cleanup_errors"], [])

    def test_unexpected_state_file_is_not_deleted(self):
        fake = FakeSmoke(self.root)
        fake.preflight()
        fake.state_dir.mkdir(parents=True)
        foreign = fake.state_dir / "unrelated"
        foreign.write_text("keep")
        fake.cleanup()
        self.assertTrue(fake.evidence["cleanup_errors"])
        self.assertEqual(foreign.read_text(), "keep")

    def test_stability_check_rejects_duplicate_execution(self):
        fake = FakeSmoke(self.root)
        fake.sandbox = self.root
        path = self.root / "fixture.jsonl"
        path.write_text('{}\n{}\n')
        with self.assertRaises(smoke.SmokeError):
            smoke.NativeSmoke.stable_count(fake, "fixture", 1, seconds=0)

    def test_partial_event_is_not_a_complete_invocation(self):
        fake = FakeSmoke(self.root)
        fake.sandbox = self.root
        (self.root / "fixture.jsonl").write_text('{"pid":1}\n{"pid":')
        self.assertEqual(fake.events("fixture"), [{"pid": 1}])

    def test_pid_existence_probe_never_sends_a_signal(self):
        with patch.object(smoke.os, "kill") as kill:
            self.assertTrue(smoke.NativeSmoke.pid_exists(12345))
        kill.assert_called_once_with(12345, 0)
        with patch.object(smoke.os, "kill", side_effect=ProcessLookupError):
            self.assertFalse(smoke.NativeSmoke.pid_exists(12345))
        for invalid in (-1, 0, "12345"):
            with self.assertRaises(smoke.SmokeError):
                smoke.NativeSmoke.pid_exists(invalid)

    def test_cli_rejects_contradictory_exit_code(self):
        fake = FakeSmoke(self.root)
        result = SimpleNamespace(returncode=1, stdout=json.dumps(FakeSmoke.envelope({})), stderr="")
        with patch.object(smoke.subprocess, "run", return_value=result), self.assertRaises(smoke.SmokeError):
            smoke.NativeSmoke.command(fake, ["doctor"])

    def test_cli_rejects_invalid_json_and_wrong_schema(self):
        for output in ("not json", '{"schema_version":"wrong","ok":true}'):
            result = SimpleNamespace(returncode=0, stdout=output, stderr="")
            # Construct separately to avoid sharing any native-home state.
            other = self.root / str(len(output))
            other.mkdir()
            fake = FakeSmoke(other)
            with patch.object(smoke.subprocess, "run", return_value=result), self.assertRaises(smoke.SmokeError):
                smoke.NativeSmoke.command(fake, ["doctor"])

    def test_failure_diagnostics_are_read_only_and_scoped_to_owned_labels(self):
        fake = FakeSmoke(self.root, "Darwin")
        fake.ids = ["owned"]
        results = [SimpleNamespace(returncode=113, stdout="", stderr="missing job"),
                   SimpleNamespace(returncode=0, stdout='disabled services = {\n"com.loboot.owned" => disabled\n"com.foreign.job" => enabled\n}\n', stderr="")]
        with patch.object(smoke.subprocess, "run", side_effect=results) as run:
            smoke.NativeSmoke.capture_failure_diagnostics(fake)
        self.assertEqual([call.args[0][1] for call in run.call_args_list], ["print", "print-disabled"])
        records = fake.evidence["failure_diagnostics"]
        self.assertEqual(records[1]["stdout"], '"com.loboot.owned" => disabled')
        self.assertNotIn("com.foreign", json.dumps(records))

    def test_failure_diagnostic_errors_do_not_block_cleanup(self):
        fake = FakeSmoke(self.root)
        fake.ids = ["owned"]
        with patch.object(smoke.subprocess, "run", side_effect=OSError("manager missing")):
            smoke.NativeSmoke.capture_failure_diagnostics(fake)
        self.assertEqual(fake.evidence["failure_diagnostics"][0]["error"], "manager missing")

    def test_linux_cleanup_requires_not_found_inactive_and_zero_pid(self):
        fake = FakeSmoke(self.root)
        for output, valid in (("LoadState=not-found\nActiveState=inactive\nMainPID=0\n", True),
                              ("LoadState=loaded\nActiveState=inactive\nMainPID=0\n", False),
                              ("LoadState=not-found\nActiveState=failed\nMainPID=0\n", False)):
            result = SimpleNamespace(returncode=4, stdout=output, stderr="")
            with patch.object(smoke.subprocess, "run", return_value=result):
                if valid:
                    smoke.NativeSmoke.native_absent(fake, "owned")
                else:
                    with self.assertRaises(smoke.SmokeError):
                        smoke.NativeSmoke.native_absent(fake, "owned")

    def test_macos_cleanup_requires_exact_missing_label(self):
        fake = FakeSmoke(self.root, "Darwin")
        for label, valid in (("owned", True), ("foreign", False)):
            result = SimpleNamespace(returncode=113, stdout="", stderr=f'Could not find service "com.loboot.{label}" in domain gui: 501\n')
            with patch.object(smoke.subprocess, "run", return_value=result):
                if valid:
                    smoke.NativeSmoke.native_absent(fake, "owned")
                else:
                    with self.assertRaises(smoke.SmokeError):
                        smoke.NativeSmoke.native_absent(fake, "owned")


if __name__ == "__main__":
    unittest.main()
