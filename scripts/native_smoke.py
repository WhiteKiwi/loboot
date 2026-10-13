#!/usr/bin/env python3
"""Opt-in lifecycle smoke for disposable GitHub-hosted Linux/macOS runners.

Never run this on a personal computer or a self-hosted runner. No manager is
created, no login/logout is performed, and no linger/account/security settings
are changed. Exit 77 means an existing native user session was unavailable, NOT
that lifecycle verification passed. macOS retains disabled overrides for only
our unique labels until GitHub destroys the disposable runner; launchctl cannot
safely erase one override, so this script never resets the whole domain.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid


class SmokeError(RuntimeError):
    pass


class UnsafeInvocation(SmokeError):
    pass


def require(condition, message):
    if not condition:
        raise SmokeError(message)


def hosted_context(allow, env, system, uid, account_home):
    """Fail closed before creating files or invoking a manager."""
    if not allow:
        raise UnsafeInvocation("--allow-hosted-native is required")
    required = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"}
    if any(env.get(key) != value for key, value in required.items()):
        raise UnsafeInvocation("only disposable GitHub-hosted Actions runners are allowed")
    if system not in ("Linux", "Darwin") or env.get("RUNNER_OS") != {"Linux": "Linux", "Darwin": "macOS"}.get(system):
        raise UnsafeInvocation("RUNNER_OS must match native Linux or macOS")
    if uid == 0:
        raise UnsafeInvocation("root/system scope is prohibited")
    home = Path(env.get("HOME", ""))
    temporary = Path(env.get("RUNNER_TEMP", ""))
    if not home.is_absolute() or not temporary.is_absolute():
        raise UnsafeInvocation("HOME and RUNNER_TEMP must be absolute directories")
    home, temporary = home.resolve(strict=True), temporary.resolve(strict=True)
    if home != Path(account_home).resolve(strict=True):
        raise UnsafeInvocation("HOME must be the current runner account's real home")
    for path in (home, temporary):
        if not path.is_dir() or path.stat().st_uid != uid:
            raise UnsafeInvocation(f"runner must own directory {path}")
    return home, temporary


def snapshot(root):
    """Capture relative file content/link targets; do not follow symbolic links."""
    entries = {}
    if not root.exists():
        return entries
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            path = Path(directory) / name
            relative = str(path.relative_to(root))
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                entries[relative] = ["symlink", os.readlink(path)]
            elif stat.S_ISDIR(mode):
                entries[relative] = ["directory"]
            elif stat.S_ISREG(mode):
                entries[relative] = ["file", hashlib.sha256(path.read_bytes()).hexdigest()]
            else:
                raise SmokeError(f"unexpected special file in native directory: {path}")
    return entries


def absent_parents(paths, home):
    missing = set()
    for path in paths:
        while path != home:
            require(home in path.parents, "resource path escapes runner home")
            require(not path.is_symlink(), f"symlinked resource directory: {path}")
            if path.exists():
                require(path.is_dir(), f"resource parent is not a directory: {path}")
            else:
                missing.add(path)
            path = path.parent
    return sorted(missing, key=lambda p: len(p.parts), reverse=True)


def existing_user_bus(env, uid):
    """Locate only an already-running current-user bus, never start one."""
    env = dict(env)
    if not env.get("DBUS_SESSION_BUS_ADDRESS"):
        runtime = Path(f"/run/user/{uid}")
        bus = runtime / "bus"
        try:
            safe = (not runtime.is_symlink() and not bus.is_symlink()
                    and runtime.is_dir() and runtime.stat().st_uid == uid
                    and stat.S_ISSOCK(bus.stat().st_mode) and bus.stat().st_uid == uid)
        except OSError:
            safe = False
        if safe:
            env["XDG_RUNTIME_DIR"] = str(runtime)
            env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={bus}"
    return env


FIXTURE = '''import json, os, signal, sys, time
log, kind, marker = sys.argv[1:]
if kind == "service":
    def stop(*_):
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        time.sleep(0.75)
        sys.exit(0)
    signal.signal(signal.SIGTERM, stop)
previous_live_pids = []
if os.path.exists(log):
    with open(log, encoding="utf-8") as stream:
        for line in stream:
            pid = json.loads(line)["pid"]
            try:
                os.kill(pid, 0)
                previous_live_pids.append(pid)
            except ProcessLookupError:
                pass
with open(log, "a", encoding="utf-8") as stream:
    stream.write(json.dumps({"pid": os.getpid(), "kind": kind, "marker": marker,
                            "previous_live_pids": previous_live_pids}) + "\\n")
    stream.flush()
    os.fsync(stream.fileno())
if kind == "fail":
    print("loboot-stdout-marker", flush=True)
    print("loboot-stderr-marker token=fixture-sensitive", file=sys.stderr, flush=True)
    sys.exit(23)
if kind == "service":
    while True:
        signal.pause()
'''


class NativeSmoke:
    def __init__(self, binary, home, temporary, system, env):
        self.binary = binary
        self.home = home
        self.temporary = temporary
        self.system = system
        self.env = dict(env, HOME=str(home), LC_ALL="C")
        self.state_dir = home / ".local/share/loboot"
        self.native_dir = home / (".config/systemd/user" if system == "Linux" else "Library/LaunchAgents")
        self.ids = []
        self.manifests = {}
        self.captures = {}
        self.before = {}
        self.missing_dirs = []
        self.sandbox = None
        self.evidence = {"schema_version": "loboot.native-smoke/v1", "status": "failed",
                         "system": system, "architecture": platform.machine(),
                         "login_activation_verified": False, "commands": [], "checks": [],
                         "cleanup_errors": [], "limitations": [
                             "No logout/login or user-manager lifetime activation was tested.",
                             "No manager was created; linger/account/security settings are unchanged."]}

    def command(self, args, env=None):
        argv = [str(self.binary), "--json", *args]
        result = subprocess.run(argv, env=env or self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=25)
        try:
            envelope = json.loads(result.stdout)
        except (ValueError, TypeError) as exc:
            raise SmokeError(f"invalid JSON from {args}: {result.stdout!r} {result.stderr!r}") from exc
        self.evidence["commands"].append({"args": args, "exit_code": result.returncode,
                                          "envelope": envelope, "stderr": result.stderr})
        require(envelope.get("schema_version") == "loboot.cli/v1", "wrong CLI schema")
        require(isinstance(envelope.get("ok"), bool), "CLI envelope has no boolean ok")
        require(envelope["ok"] == (result.returncode == 0), "CLI exit status contradicts envelope")
        return envelope

    def cli(self, *args, error=None, env=None):
        envelope = self.command(list(args), env)
        if error:
            require(not envelope["ok"] and envelope.get("error", {}).get("code") == error,
                    f"{args}: expected error {error}, got {envelope}")
            return envelope["error"]
        require(envelope["ok"], f"{args}: {envelope.get('error')}")
        return envelope["data"]

    def status(self, id):
        return self.cli("status", id)["native"]

    def check(self, name):
        self.evidence["checks"].append(name)

    def wait(self, description, predicate, seconds=15):
        deadline = time.monotonic() + seconds
        while True:
            if predicate():
                return
            if time.monotonic() >= deadline:
                raise SmokeError(f"timed out waiting for {description}")
            time.sleep(0.2)

    def events(self, id):
        path = self.sandbox / f"{id}.jsonl"
        if not path.exists():
            return []
        text = path.read_text(encoding="utf-8")
        # The writer may be between write and close; retry a partial final line.
        return [json.loads(line) for line in text.splitlines(keepends=True) if line.endswith("\n")]

    def stable_count(self, id, count, seconds=3):
        deadline = time.monotonic() + seconds
        while True:
            require(len(self.events(id)) == count, f"unexpected invocation count for {id}")
            if time.monotonic() >= deadline:
                return
            time.sleep(0.1)

    @staticmethod
    def pid_exists(pid):
        require(isinstance(pid, int) and pid > 0, "fixture recorded an invalid PID")
        try:
            os.kill(pid, 0)  # Existence probe only. Never signal an unverified PID.
            return True
        except ProcessLookupError:
            return False

    def processes_exited(self, id):
        self.wait("fixture processes exited", lambda: all(
            not self.pid_exists(event["pid"]) for event in self.events(id)))

    def manifest(self, id, kind, marker):
        executable = Path(sys.executable).resolve(strict=True)
        value = {"id": id, "executable": str(executable),
                 "args": [str(self.sandbox / "fixture.py"), str(self.sandbox / f"{id}.jsonl"), kind, marker],
                 "cwd": str(self.sandbox), "mode": "service" if kind == "service" else "once",
                 "restart": "always" if kind == "service" else "never"}
        path = self.sandbox / f"{id}.toml"
        path.write_text("\n".join(f"{key} = {json.dumps(value)}" for key, value in value.items()) + "\n", encoding="utf-8")
        self.manifests.setdefault(id, []).append(value)
        return path

    def add(self, kind):
        id = f"loboot-e2e-{uuid.uuid4().hex}"
        self.ids.append(id)
        path = self.manifest(id, kind, "initial")
        self.cli("add", str(path))
        self.cli("add", str(path))
        if self.system == "Darwin":
            record = self.cli("show", id)["service"]
            generation = record.get("log_generation")
            require(record.get("schema_version") == 2 and isinstance(generation, str)
                    and re.fullmatch(r"[0-9a-f]{32}", generation), "missing owned capture generation")
            self.captures[id] = generation
        status = self.status(id)
        require(status["enabled"] is False and status["running"] is False,
                "add enabled persistence or started a process")
        if self.system == "Darwin":
            require(status["loaded"] is False, "add loaded a launchd job")
        self.stable_count(id, 0, 0.5)
        self.check(f"{kind}: add and identical add did not start or enable")
        return id

    def preflight(self):
        require(not self.state_dir.exists() and not self.state_dir.is_symlink(),
                "runner already has loboot state; refusing shared state")
        self.missing_dirs = absent_parents([self.state_dir, self.native_dir], self.home)
        self.before = snapshot(self.native_dir)
        doctor = self.cli("doctor")
        require(doctor.get("state", {}).get("ok") is True, "doctor found invalid state")
        session = doctor.get("session", {})
        require(session.get("checked") is True, "doctor did not check native session")
        if session.get("available") is not True:
            self.evidence.update(status="skipped", reason=session)
            return False
        self.check("existing native user session available")
        return True

    def lifecycle(self):
        self.sandbox = Path(tempfile.mkdtemp(prefix="loboot-native-", dir=self.temporary)).resolve()
        self.evidence["fixture_directory"] = str(self.sandbox)
        (self.sandbox / "fixture.py").write_text(FIXTURE, encoding="utf-8")
        once = self.add("once")
        if self.system == "Darwin":
            self.cli("start", once, error="disabled_service")
            self.cli("restart", once, error="disabled_service")
            self.stable_count(once, 0, 0.5)
            self.check("macOS disabled start and restart rejected without execution")
        self.cli("enable", once)
        self.cli("enable", once)
        require(self.status(once)["enabled"] is True, "enable not observed")
        self.stable_count(once, 0, 1)
        self.check("enable and repeated enable did not start once job")
        self.cli("start", once)
        self.wait("one once invocation", lambda: len(self.events(once)) == 1)
        self.wait("once process exit", lambda: self.status(once)["running"] is False)
        self.stable_count(once, 1)
        self.check("fresh once start executed exactly once and exited")
        self.stop_disable(once)
        self.stable_count(once, 1)
        if self.system == "Linux":
            self.cli("start", once)
            self.wait("disabled Linux once invocation", lambda: len(self.events(once)) == 2)
            require(self.status(once)["enabled"] is False, "start changed Linux persistence")
            self.cli("stop", once)
            self.check("Linux explicit start preserved disabled persistence")
            missing = dict(self.env, DBUS_SESSION_BUS_ADDRESS=f"unix:path={self.sandbox}/missing-bus",
                           XDG_RUNTIME_DIR=str(self.sandbox))
            self.cli("status", once, error="session_unavailable", env=missing)
            self.check("Linux missing-session status reports session_unavailable")
        else:
            self.cli("start", once, error="disabled_service")
        self.remove(once)

        service = self.add("service")
        self.cli("enable", service)
        self.cli("start", service)
        self.wait("running service", lambda: self.status(service)["running"] is True and len(self.events(service)) == 1)
        self.cli("start", service)
        self.stable_count(service, 1)
        if self.system == "Darwin":
            self.cli("disable", service, error="stop_first")
            require(self.status(service)["enabled"] is True and self.status(service)["running"] is True,
                    "loaded-disable failure changed macOS state")
            self.check("macOS loaded-disable rejected without changing runtime or preference")
        for count in range(2, 5):
            self.cli("restart", service)
            self.wait("one restart invocation", lambda: len(self.events(service)) == count and self.status(service)["running"] is True)
            self.stable_count(service, count)
            require(self.events(service)[-1].get("previous_live_pids") == [],
                    "restart overlapped a previous fixture process")
            require(all(not self.pid_exists(event["pid"]) for event in self.events(service)[:-1]),
                    "restart returned with an old fixture process still alive")
        self.evidence["service_restart_cycles"] = 3
        self.evidence["fixture_term_delay_seconds"] = 0.75
        self.check("three service restarts with delayed SIGTERM exit ran exactly once each without overlap")
        self.cli("stop", service)
        self.cli("stop", service)
        self.wait("service stopped", lambda: self.status(service)["running"] is False)
        self.processes_exited(service)
        require(self.status(service)["enabled"] is True, "stop changed persistence")
        self.stable_count(service, 4)
        self.check("stop and repeated stop suppressed KeepAlive/Restart without disabling")
        updated = self.manifest(service, "service", "updated")
        self.cli("update", str(updated))
        self.stable_count(service, 4, 1)
        self.cli("start", service)
        self.wait("updated service", lambda: len(self.events(service)) == 5 and self.status(service)["running"] is True)
        require(self.events(service)[-1]["marker"] == "updated", "updated argv not used")
        self.check("stopped definition update did not start; later start used new argv")
        self.stop_disable(service)
        self.stable_count(service, 5)
        self.remove(service)

        failing = self.add("fail")
        self.cli("enable", failing)
        envelope = self.command(["start", failing])
        if self.system == "Linux":
            require(not envelope["ok"] and envelope.get("error", {}).get("code") == "partial_failure",
                    "failed Linux oneshot did not return partial_failure")
        else:
            require(envelope["ok"], "launchd did not accept failing fixture bootstrap")
        self.wait("failing fixture invocation", lambda: len(self.events(failing)) == 1)
        key = "ExecMainStatus" if self.system == "Linux" else "last exit code"
        self.wait("native exit code 23", lambda: self.status(failing)["running"] is False
                  and self.status(failing)["raw"].get(key) == "23")
        self.stable_count(failing, 1)
        self.check("failing once fixture observed native exit 23; no implicit retry")
        def logs_ready():
            sources = self.cli("logs", failing, "--lines", "100", "--max-bytes", "65536")["sources"]
            content = "\n".join(source["text"] for source in sources)
            require("fixture-sensitive" not in content, "common token was not redacted")
            return "loboot-stdout-marker" in content and "loboot-stderr-marker" in content
        self.wait("captured stdout/stderr markers", logs_ready)
        bounded = self.cli("logs", failing, "--lines", "1", "--max-bytes", "16")["sources"]
        require(all(source["bytes_returned"] <= 16 and source["lines_returned"] <= 1 for source in bounded),
                "log response exceeded requested bounds")
        require(any(hint["code"] == "native_exit" for hint in self.cli("status", failing)["diagnostics"]),
                "failure status omitted its fixed native-exit hint")
        self.check("stdout/stderr captured, common token redacted, log reads bounded, and exit diagnostics preserved")
        self.stop_disable(failing)
        self.remove(failing)

    def stop_disable(self, id):
        self.cli("stop", id)
        self.cli("stop", id)
        self.wait("stopped job", lambda: self.status(id)["running"] is False)
        self.processes_exited(id)
        self.cli("disable", id)
        self.cli("disable", id)
        status = self.status(id)
        require(status["running"] is False and status["enabled"] is False,
                "stop/disable did not leave job stopped and disabled")
        if self.system == "Darwin":
            require(status["loaded"] is False, "macOS stop left job loaded")
        elif status["raw"].get("ActiveState") == "failed":
            # systemd retains failed units after removal until their failed state
            # is reset. Clear only our exact owned fixture, after recording exit.
            command = ["systemctl", "--user", "reset-failed", "--", f"loboot-{id}.service"]
            result = subprocess.run(command, env=self.env, text=True, capture_output=True, timeout=25)
            self.evidence.setdefault("cleanup_actions", []).append({"args": command, "exit_code": result.returncode,
                                                                   "stdout": result.stdout, "stderr": result.stderr})
            require(result.returncode == 0, f"failed to clear owned fixture failure: {result.stderr}")

    def remove(self, id):
        self.cli("remove", id)
        self.cli("remove", id)
        self.cli("show", id, error="not_managed")

    def native_absent(self, id):
        """Read native state after remove, without relying on the ownership record."""
        if self.system == "Linux":
            command = ["systemctl", "--user", "show", "--no-pager",
                       "--property=LoadState,ActiveState,MainPID", "--", f"loboot-{id}.service"]
        else:
            command = ["/bin/launchctl", "print", f"gui/{os.getuid()}/com.loboot.{id}"]
        result = subprocess.run(command, env=self.env, text=True, capture_output=True, timeout=25)
        self.evidence.setdefault("cleanup_queries", []).append({"args": command, "exit_code": result.returncode,
                                                                "stdout": result.stdout, "stderr": result.stderr})
        if self.system == "Linux":
            values = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
            require(result.returncode in (0, 1, 4) and values.get("LoadState") == "not-found"
                    and values.get("ActiveState") == "inactive" and values.get("MainPID") == "0",
                    f"native job not confirmed absent: {id}: {result.stdout} {result.stderr}")
        else:
            require(result.returncode in (3, 113) and f'Could not find service "com.loboot.{id}" in domain' in result.stderr,
                    f"launchd job not confirmed absent: {id}: {result.stdout} {result.stderr}")

    def capture_failure_diagnostics(self):
        """Best-effort, read-only native output scoped to this run's labels."""
        commands = []
        for id in self.ids:
            if self.system == "Linux":
                commands.append(["systemctl", "--user", "show", "--no-pager",
                                 "--property=LoadState,ActiveState,SubState,UnitFileState,MainPID,ExecMainStatus,Result,FragmentPath,DropInPaths",
                                 "--", f"loboot-{id}.service"])
            else:
                commands.append(["/bin/launchctl", "print", f"gui/{os.getuid()}/com.loboot.{id}"])
        if self.system == "Darwin" and self.ids:
            commands.append(["/bin/launchctl", "print-disabled", f"gui/{os.getuid()}"])
        for command in commands:
            record = {"args": command}
            try:
                result = subprocess.run(command, env=self.env, text=True, capture_output=True, timeout=5)
                stdout = result.stdout
                if "print-disabled" in command:
                    # print-disabled has no single-label form. Retain raw lines
                    # for only our labels, not unrelated runner overrides.
                    labels = [f'"com.loboot.{id}"' for id in self.ids]
                    stdout = "\n".join(line for line in stdout.splitlines()
                                       if any(label in line for label in labels))
                    record["stdout_scope"] = "owned-label lines only"
                record.update(exit_code=result.returncode, stdout=stdout, stderr=result.stderr)
            except (OSError, subprocess.TimeoutExpired) as exc:
                record["error"] = str(exc)
            self.evidence.setdefault("failure_diagnostics", []).append(record)

    def cleanup_captures(self):
        """Delete only exact, proven fixture-owned capture generations after exit."""
        root = self.state_dir / "logs"
        if not root.exists() and not root.is_symlink():
            require(not self.captures, "expected fixture captures are missing")
            return
        def owned(path, directory=False):
            info = path.lstat()
            require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == (0o700 if directory else 0o600)
                    and (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode) and info.st_nlink == 1),
                    f"unsafe fixture log path: {path}")
        owned(root, True)
        expected = {f"{id}-{generation}" for id, generation in self.captures.items()}
        require({p.name for p in root.iterdir()} == expected | {".owner"}, "unexpected retained capture generation")
        owned(root / ".owner")
        require((root / ".owner").read_bytes() == b"loboot.private-log-root/v1\n", "unexpected capture-root marker")
        for id, generation in self.captures.items():
            directory = root / f"{id}-{generation}"
            owned(directory, True)
            require({p.name for p in directory.iterdir()} == {".owner", "stdout.log", "stderr.log"},
                    "unexpected fixture capture contents")
            for name in (".owner", "stdout.log", "stderr.log"):
                owned(directory / name)
            require((directory / ".owner").read_text() == f"loboot.private-log-generation/v1\n{id}\n{generation}\n",
                    "fixture generation ownership changed")
            for name in ("stdout.log", "stderr.log", ".owner"):
                (directory / name).unlink()
            directory.rmdir()
        (root / ".owner").unlink()
        root.rmdir()

    def cleanup(self):
        for id in reversed(self.ids):
            try:
                record = self.state_dir / f"{id}.toml"
                if record.exists() or record.is_symlink():
                    # Refuse any native fallback when ownership/drift validation fails.
                    actual = self.cli("show", id)["service"]["manifest"]
                    require(actual in self.manifests[id], f"owned fixture record changed: {id}")
                    self.stop_disable(id)
                    self.remove(id)
                self.native_absent(id)
                self.processes_exited(id)
            except (SmokeError, OSError, subprocess.TimeoutExpired) as exc:
                self.evidence["cleanup_errors"].append(str(exc))
        if not self.evidence["cleanup_errors"]:
            try:
                if self.state_dir.exists():
                    self.cleanup_captures()
                    require(set(p.name for p in self.state_dir.iterdir()) <= {".lock"}, "unexpected owned state remains")
                    (self.state_dir / ".lock").unlink(missing_ok=True)
                # systemctl may create an empty activation directory. Remove
                # it before its parents, only if it was absent and remains empty.
                wants = self.native_dir / "default.target.wants"
                if "default.target.wants" not in self.before and wants.is_dir() and not wants.is_symlink():
                    wants.rmdir()
                # Only delete previously absent directories, and only when empty.
                for path in self.missing_dirs:
                    if path.exists():
                        path.rmdir()
                require(snapshot(self.native_dir) == self.before, "unrelated native directory entries changed")
                if self.sandbox is not None:
                    shutil.rmtree(self.sandbox)
                self.check("owned records, definitions, activation links and runtime jobs removed; unrelated files unchanged")
            except (SmokeError, OSError) as exc:
                self.evidence["cleanup_errors"].append(str(exc))
        if self.system == "Darwin" and self.ids:
            self.evidence["limitations"].append("Unique test labels remain disabled in launchd until this disposable runner is destroyed; no domain-wide reset was performed.")
            self.evidence["disabled_override_labels"] = [f"com.loboot.{id}" for id in self.ids]
        if self.evidence["cleanup_errors"]:
            self.evidence["status"] = "failed"

    def run(self):
        if not self.preflight():
            return 77
        try:
            self.lifecycle()
            self.evidence["status"] = "passed"
        except (SmokeError, OSError, subprocess.TimeoutExpired) as exc:
            self.evidence["error"] = str(exc)
            self.capture_failure_diagnostics()
        finally:
            self.cleanup()
        if self.evidence["cleanup_errors"] and "error" not in self.evidence:
            self.capture_failure_diagnostics()
        return 0 if self.evidence["status"] == "passed" else 1


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--allow-hosted-native", action="store_true")
    parser.add_argument("--report", type=Path, help="JSON evidence destination (never implies login verification)")
    args = parser.parse_args(argv)
    evidence = {"schema_version": "loboot.native-smoke/v1", "status": "failed", "login_activation_verified": False}
    smoke = None
    try:
        home, temporary = hosted_context(args.allow_hosted_native, os.environ, platform.system(), os.getuid(), pwd.getpwuid(os.getuid()).pw_dir)
        binary = args.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "binary must be an executable regular file")
        env = existing_user_bus(os.environ, os.getuid()) if platform.system() == "Linux" else dict(os.environ)
        smoke = NativeSmoke(binary, home, temporary, platform.system(), env)
        def interrupted(signum, frame):
            raise SmokeError(f"interrupted by signal {signum}; attempting owned-resource cleanup")
        signal.signal(signal.SIGTERM, interrupted)
        signal.signal(signal.SIGINT, interrupted)
        code = smoke.run()
        evidence = smoke.evidence
    except UnsafeInvocation as exc:
        code, evidence["error"] = 2, str(exc)
    except (SmokeError, OSError, subprocess.TimeoutExpired) as exc:
        code = 1
        evidence = smoke.evidence if smoke else evidence
        evidence["error"] = str(exc)
    if args.report:
        args.report.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(evidence, indent=2))
    return code


if __name__ == "__main__":
    sys.exit(main())
