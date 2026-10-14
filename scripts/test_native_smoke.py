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
import sys
import time
import tempfile
import tomllib
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

    def write_record(self, id, manifest):
        record = {"schema_version": 2 if self.system == "Darwin" else 1,
                  "native_name": self.native_name(id), "native_bytes": json.dumps(manifest, ensure_ascii=False)}
        if self.jobs[id].get("generation"):
            record["log_generation"] = self.jobs[id]["generation"]
        lines = [f"{key} = {json.dumps(value, ensure_ascii=False)}" for key, value in record.items()]
        lines += ["", "[manifest]"]
        lines += [f"{key} = {json.dumps(value, ensure_ascii=False)}" for key, value in manifest.items() if key != "environment"]
        lines += ["", "[manifest.environment]"]
        lines += [f"{key} = {json.dumps(value, ensure_ascii=False)}" for key, value in manifest.get("environment", {}).items()]
        (self.state_dir / f"{id}.toml").write_text("\n".join(lines) + "\n", encoding="utf-8")
        (self.native_dir / self.native_name(id)).write_text(record["native_bytes"], encoding="utf-8")

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
        action, id = args[:2]
        if action in ("add", "update"):
            manifest = tomllib.loads(Path(id).read_text(encoding="utf-8"))
            id = manifest["id"]
            if id not in self.jobs:
                self.jobs[id] = {"enabled": False, "loaded": self.system == "Linux", "running": False, "raw": {}}
            self.jobs[id]["manifest"] = manifest
            self.state_dir.mkdir(parents=True, exist_ok=True)
            self.native_dir.mkdir(parents=True, exist_ok=True)
            (self.state_dir / ".lock").touch()
            if self.system == "Darwin" and "generation" not in self.jobs[id]:
                generation = smoke.uuid.uuid4().hex
                self.jobs[id]["generation"] = generation
                root = self.state_dir / "logs"
                root.mkdir(mode=0o700, exist_ok=True)
                marker = root / ".owner"
                marker.write_bytes(b"loboot.private-log-root/v1\n"); marker.chmod(0o600)
                directory = root / f"{id}-{generation}"
                directory.mkdir(mode=0o700)
                for name, content in ((".owner", f"loboot.private-log-generation/v1\n{id}\n{generation}\n"), ("stdout.log", ""), ("stderr.log", "")):
                    file = directory / name; file.write_text(content); file.chmod(0o600)
            self.write_record(id, manifest)
            return self.envelope({})
        if id not in self.jobs:
            return self.envelope({}) if action == "remove" else self.error("not_managed")
        job = self.jobs[id]
        if action == "show":
            try:
                record = tomllib.loads((self.state_dir / f"{id}.toml").read_text(encoding="utf-8"))
            except (tomllib.TOMLDecodeError, UnicodeDecodeError):
                return self.error("invalid_state")
            if not isinstance(record.get("manifest"), dict):
                return self.error("invalid_state")
            environment = record["manifest"].get("environment", {})
            record["manifest"]["environment"] = {key: "[REDACTED]" for key in environment}
            record["native_bytes"] = ("Environment values redacted; display only, not replayable.\n"
                                       + json.dumps(record["manifest"], ensure_ascii=False))
            return self.envelope({"service": record, "environment_redacted": bool(environment)})
        if action == "logs":
            size = int(args[args.index("--max-bytes") + 1]) if "--max-bytes" in args else 65536
            content = "loboot-stdout-marker\nloboot-stderr-marker token=[REDACTED]\n"[:size]
            if "--lines" in args and args[args.index("--lines") + 1] == "1":
                content = content.splitlines(keepends=True)[0]
            return self.envelope({"sources": [{"text": content, "bytes_returned": len(content), "lines_returned": len(content.splitlines())}]})
        if action == "status":
            return self.envelope({"native": job.copy(), "diagnostics": [{"code": "native_exit"}] if job["raw"].get("ExecMainStatus") == "23" else []})
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
                    file.write(json.dumps({"pid": 987654, "kind": kind, "marker": marker, "previous_live_pids": [],
                                           "environment": dict(job["manifest"].get("environment", {}))}) + "\n")
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

    def owned_fixture(self):
        fake = FakeSmoke(self.root)
        self.assertTrue(fake.preflight())
        fake.sandbox = self.root / "sandbox"
        fake.sandbox.mkdir()
        id = fake.add("once")
        return fake, id, fake.state_dir / f"{id}.toml"

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
        self.assertEqual(fake.evidence["service_restart_cycles"], 3)
        self.assertEqual(fake.evidence["fixture_term_delay_seconds"], 0.75)
        self.assertEqual(fake.evidence["checks"].count(
            "fixture environment literals and PATH preserved on every invocation"), 3)
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
        self.assertEqual(fake.evidence["checks"].count(
            "fixture environment literals and PATH preserved on every invocation"), 3)
        self.assertFalse(fake.native_dir.exists())

    def test_fixture_manifest_roundtrips_literal_environment_without_changing_helper_path(self):
        fake = FakeSmoke(self.root)
        fake.sandbox = self.root
        fake.env["PATH"] = "/usr/bin:/bin"
        for marker in ("initial", "updated"):
            path = fake.manifest("fixture", "service", marker)
            manifest = tomllib.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(manifest["environment"], dict(smoke.FIXTURE_ENVIRONMENT,
                                                         LOBOOT_FIXTURE_REVISION=marker))
            self.assertTrue(all(Path(component).is_absolute()
                                for component in manifest["environment"]["PATH"].split(":")))
            self.assertEqual(fake.env["PATH"], "/usr/bin:/bin")
        self.assertIn("[environment]", path.read_text())

    def test_fixture_logs_only_selected_environment_and_preserves_literals(self):
        script = self.root / "fixture.py"
        script.write_text(smoke.FIXTURE, encoding="utf-8")
        log = self.root / "fixture.jsonl"
        environment = dict(os.environ, **smoke.FIXTURE_ENVIRONMENT,
                           LOBOOT_UNRELATED_PRIVATE_VALUE="must-not-be-logged")
        result = subprocess.run([sys.executable, str(script), str(log), "once", "initial"],
                                env=environment, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        event = json.loads(log.read_text())
        self.assertEqual(event["environment"], smoke.FIXTURE_ENVIRONMENT)
        self.assertNotIn("must-not-be-logged", log.read_text())
        self.assertNotIn("LOBOOT_UNRELATED_PRIVATE_VALUE", log.read_text())

    def test_environment_verification_rejects_missing_expanded_or_stale_values(self):
        fake = FakeSmoke(self.root)
        fake.sandbox = self.root
        fake.manifest("fixture", "service", "initial")
        fake.manifest("fixture", "service", "updated")
        initial = dict(smoke.FIXTURE_ENVIRONMENT)
        updated = dict(initial, LOBOOT_FIXTURE_REVISION="updated")
        events = [{"marker": "initial", "environment": initial},
                  {"marker": "updated", "environment": updated}]
        with patch.object(fake, "events", return_value=events):
            fake.verify_environment("fixture")
        bad_events = [
            {"marker": "initial"},
            {"marker": "initial", "environment": dict(initial, LOBOOT_FIXTURE_DOLLARS="/runner expanded")},
            {"marker": "initial", "environment": dict(initial, LOBOOT_FIXTURE_SPECIFIERS="fixture % /home")},
            {"marker": "initial", "environment": dict(initial, PATH="/usr/bin:/bin")},
            {"marker": "updated", "environment": initial},
            {"marker": "unknown", "environment": initial},
        ]
        for event in bad_events:
            with self.subTest(event=event), patch.object(fake, "events", return_value=[event]):
                with self.assertRaisesRegex(smoke.SmokeError, "environment literals or PATH changed"):
                    fake.verify_environment("fixture")

    def test_lifecycle_fails_when_environment_probe_is_wrong(self):
        fake = FakeSmoke(self.root)
        original_events = fake.events

        def changed_events(id):
            events = original_events(id)
            for event in events:
                event["environment"]["LOBOOT_FIXTURE_DOLLARS"] = "expanded instead of literal"
            return events

        with patch.object(fake, "events", side_effect=changed_events):
            self.assertEqual(fake.run(), 1)
        self.assertIn("environment literals or PATH changed", fake.evidence["error"])
        self.assertEqual(fake.evidence["cleanup_errors"], [])
        self.assertEqual(fake.jobs, {})

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

    def test_capture_cleanup_refuses_unknown_files_and_symlinks(self):
        for kind in ("extra", "symlink"):
            root = self.root / kind
            root.mkdir()
            fake = FakeSmoke(root, "Darwin")
            self.assertTrue(fake.preflight())
            fake.lifecycle()
            id, generation = next(iter(fake.captures.items()))
            directory = fake.state_dir / "logs" / f"{id}-{generation}"
            outside = root / "unrelated"
            outside.write_text("untouched")
            if kind == "extra":
                (directory / "unexpected").write_text("foreign")
            else:
                (directory / "stdout.log").unlink()
                (directory / "stdout.log").symlink_to(outside)
            fake.cleanup()
            self.assertTrue(fake.evidence["cleanup_errors"])
            self.assertEqual(outside.read_text(), "untouched")
            self.assertTrue(directory.exists())

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

    def test_cleanup_uses_exact_owned_record_despite_show_redaction(self):
        fake, id, path = self.owned_fixture()
        show = fake.cli("show", id)
        self.assertTrue(show["environment_redacted"])
        self.assertEqual(set(show["service"]["manifest"]["environment"].values()), {"[REDACTED]"})
        self.assertIn("display only, not replayable", show["service"]["native_bytes"])
        self.assertEqual(smoke.read_owned_record(fake.home, path)["manifest"], fake.manifests[id][0])
        fake.cleanup()
        self.assertEqual(fake.evidence["cleanup_errors"], [])
        self.assertEqual(fake.jobs, {})

    def test_cleanup_rejects_environment_mismatch_hidden_by_identical_redaction(self):
        fake, id, path = self.owned_fixture()
        before_show = fake.cli("show", id)
        changed = json.loads(json.dumps(fake.manifests[id][0]))
        changed["environment"]["LOBOOT_FIXTURE_DOLLARS"] = "foreign literal"
        fake.write_record(id, changed)
        self.assertEqual(fake.cli("show", id), before_show)
        before = len(fake.calls)
        fake.cleanup()
        self.assertEqual(fake.calls[before:], [["show", id]])
        self.assertTrue(any("owned fixture record changed" in error for error in fake.evidence["cleanup_errors"]))
        self.assertTrue(path.exists())
        self.assertIn(id, fake.jobs)

    def test_owned_record_refuses_symlinks_hardlinks_special_files_and_oversize(self):
        fake, id, path = self.owned_fixture()
        content = path.read_bytes()
        outside = self.root / "outside.toml"
        outside.write_bytes(content)
        for kind in ("symlink", "hardlink", "directory", "fifo", "oversize"):
            with self.subTest(kind=kind):
                path.unlink()
                if kind == "symlink":
                    path.symlink_to(outside)
                elif kind == "hardlink":
                    os.link(outside, path)
                elif kind == "directory":
                    path.mkdir()
                elif kind == "fifo":
                    os.mkfifo(path)
                else:
                    with path.open("wb") as stream:
                        stream.truncate(smoke.OWNED_RECORD_LIMIT + 1)
                with patch.object(smoke.os, "read") as read:
                    with self.assertRaises((smoke.SmokeError, OSError)):
                        smoke.read_owned_record(fake.home, path)
                    read.assert_not_called()
                if path.is_dir() and not path.is_symlink():
                    path.rmdir()
                else:
                    path.unlink()
                path.write_bytes(content)
        self.assertEqual(outside.read_bytes(), content)

    def test_owned_record_refuses_foreign_owner_and_writable_records(self):
        fake, id, path = self.owned_fixture()
        real_fstat = smoke.os.fstat
        inode = path.stat().st_ino

        def foreign_owner(fd):
            info = real_fstat(fd)
            if info.st_ino == inode:
                fields = {key: getattr(info, key) for key in dir(info) if key.startswith("st_")}
                return SimpleNamespace(**dict(fields, st_uid=os.getuid() + 1))
            return info

        with patch.object(smoke.os, "fstat", side_effect=foreign_owner):
            with self.assertRaisesRegex(smoke.SmokeError, "unsafe fixture ownership record"):
                smoke.read_owned_record(fake.home, path)
        path.chmod(0o666)
        with self.assertRaisesRegex(smoke.SmokeError, "unsafe fixture ownership record"):
            smoke.read_owned_record(fake.home, path)

    def test_owned_record_refuses_symlinked_parent(self):
        fake, id, path = self.owned_fixture()
        moved = fake.state_dir.with_name("moved")
        fake.state_dir.rename(moved)
        fake.state_dir.symlink_to(moved, target_is_directory=True)
        with self.assertRaises(OSError):
            smoke.read_owned_record(fake.home, path)

    def test_owned_record_refuses_changes_during_read(self):
        fake, id, path = self.owned_fixture()
        original = path.read_bytes()
        real_read = smoke.os.read
        for replacement in (False, True):
            with self.subTest(replacement=replacement):
                changed = False

                def racing_read(fd, size):
                    nonlocal changed
                    data = real_read(fd, size)
                    if not changed:
                        changed = True
                        if replacement:
                            path.unlink()
                        path.write_bytes(original + b" ")
                    return data

                with patch.object(smoke.os, "read", side_effect=racing_read):
                    with self.assertRaisesRegex(smoke.SmokeError, "record changed"):
                        smoke.read_owned_record(fake.home, path)
                path.write_bytes(original)

    def test_owned_record_refuses_invalid_toml_utf8_or_missing_manifest(self):
        fake, id, path = self.owned_fixture()
        for content in (b"\xff", b"not valid toml", b"schema_version = 1\n"):
            with self.subTest(content=content):
                path.write_bytes(content)
                with self.assertRaises(smoke.SmokeError):
                    smoke.read_owned_record(fake.home, path)

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

    def test_owned_fixture_delays_sigterm_before_exit(self):
        script = self.root / "fixture.py"
        script.write_text(smoke.FIXTURE)
        log = self.root / "fixture.jsonl"
        child = subprocess.Popen([sys.executable, str(script), str(log), "service", "test"])
        try:
            deadline = time.monotonic() + 5
            while not log.exists() or not log.read_text().endswith("\n"):
                self.assertIsNone(child.poll())
                self.assertLess(time.monotonic(), deadline)
                time.sleep(0.01)
            started = time.monotonic()
            child.terminate()
            self.assertEqual(child.wait(timeout=5), 0)
            self.assertGreaterEqual(time.monotonic() - started, 0.6)
            events = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(events), 1)
            self.assertEqual(events[0]["previous_live_pids"], [])
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)

    def test_owned_fixture_reports_preexisting_process_overlap(self):
        script = self.root / "fixture.py"
        script.write_text(smoke.FIXTURE)
        log = self.root / "fixture.jsonl"
        log.write_text(json.dumps({"pid": os.getpid()}) + "\n")
        result = subprocess.run([sys.executable, str(script), str(log), "once", "test"], capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(log.read_text().splitlines()[-1])["previous_live_pids"], [os.getpid()])

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
