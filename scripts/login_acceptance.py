#!/usr/bin/env python3
"""Offline template and consistency checks for manually collected relogin evidence.

This tool never runs loboot, a native manager, or a logout command. A consistent
operator report is not independent proof that a real session transition occurred.
See docs/LOGIN-ACCEPTANCE.md before performing any live step.
"""
from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import uuid

SCHEMA = "loboot.login-acceptance/v1"
STAGES = ("armed", "activated", "disabled", "returned", "observed")
MODES = ("once", "service")
MIN_OBSERVATION_SECONDS = 30


class EvidenceError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise EvidenceError(message)


def text(value, name):
    require(isinstance(value, str) and bool(value.strip()), f"missing {name}")
    return value


def integer(value, name, minimum=0):
    require(type(value) is int and value >= minimum, f"invalid {name}")
    return value


def timestamp(value):
    try:
        parsed = datetime.fromisoformat(text(value, "timestamp").replace("Z", "+00:00"))
    except ValueError as exc:
        raise EvidenceError("invalid timestamp") from exc
    require(parsed.tzinfo is not None and parsed.utcoffset() is not None,
            "timestamps must include a timezone")
    return parsed


def read_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f"duplicate JSON key: {key}")
            result[key] = value
        return result
    try:
        return json.loads(raw, object_pairs_hook=unique)
    except (ValueError, UnicodeError) as exc:
        raise EvidenceError(f"invalid JSON: {exc}") from exc


def read_regular(path, limit=4 * 1024 * 1024):
    """Bounded, descriptor-relative read; never follow even an ancestor symlink."""
    path = path.absolute()
    require(".." not in path.parts, "traversing input path")
    directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        descriptor = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        try:
            info = os.fstat(descriptor)
            require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == os.getuid(),
                    "input must be an owned single-link regular file")
            require(info.st_size <= limit, "input exceeds 4 MiB")
            chunks, size = [], 0
            while size <= limit:
                chunk = os.read(descriptor, min(65536, limit + 1 - size))
                if not chunk:
                    break
                chunks.append(chunk)
                size += len(chunk)
            require(size <= limit, "input exceeds 4 MiB")
            return b"".join(chunks)
        finally:
            os.close(descriptor)
    finally:
        os.close(directory)


def artifact(root, reference, *, empty=False):
    """Only read a bounded regular nonsymlink file inside the evidence directory."""
    require(isinstance(reference, dict), "missing artifact reference")
    relative = Path(text(reference.get("path"), "artifact path"))
    require(not relative.is_absolute() and ".." not in relative.parts
            and relative.parts, "artifact path must stay inside evidence directory")
    raw = read_regular(root / relative)
    require(empty or bool(raw.strip()), "empty artifact")
    require(hashlib.sha256(raw).hexdigest() == reference.get("sha256"),
            f"artifact hash mismatch: {relative}")
    return raw


def template(system, run_id):
    require(system in ("Linux", "Darwin"), "unsupported system")
    require(re.fullmatch(r"[0-9a-f]{32}", run_id) is not None, "invalid run_id")
    return {
        "schema_version": SCHEMA, "status": "not_run",
        "reason": "Real logout/login has not been performed.",
        "provenance": "manual-isolated-account", "run_id": run_id,
        "host": {"system": system, "os_version": None, "architecture": None,
                 "uid": None, "home": None, "boot_identity": None,
                 "linger": None if system == "Linux" else "not_applicable"},
        "build": {"revision": None, "binary_sha256": None, "version": None},
        "authorization": {"isolated_account": False, "native_changes": False,
                          "two_logout_login_cycles": False, "reference": None},
        "fixture_directory": None, "python": None, "fixture_source": None,
        "fixtures": {mode: {"id": f"loboot-login-{run_id}-{mode}", "show": None}
                     for mode in MODES},
        "boundaries": {name: {"real_logout_login": False, "no_manual_activation": False,
                              "before_identity": None, "after_identity": None,
                              "logout_at": None, "absent_at": None, "login_at": None,
                              "boot_identity": None, "session_absent_observed": False,
                              "observer": None, "proof": None}
                       for name in ("enabled", "disabled")},
        "observations": {stage: {"at": None, "jobs": {
            mode: {"status": None, "events": None} for mode in MODES}} for stage in STAGES},
        "command_transcript": None,
        "cleanup": {"at": None, "only_test_owned": False, "jobs_absent": False,
                    "processes_exited": False, "removed_ids": [], "errors": [],
                    "native_before": None, "native_after": None,
                    "state_before": None, "state_after": None,
                    "proof": None, "retained_disabled_labels": []},
    }


def validate(report, root):
    require(isinstance(report, dict) and report.get("schema_version") == SCHEMA,
            "not a login-acceptance/v1 report (hosted smoke is not relogin evidence)")
    require(report.get("provenance") == "manual-isolated-account", "wrong provenance")
    status = report.get("status")
    require(status in ("not_run", "blocked", "failed", "passed"), "invalid status")
    if status != "passed":
        text(report.get("reason"), "non-pass reason")
        return {"reported_status": status, "consistent": False,
                "independently_verified": False, "reason": report["reason"]}

    run_id = text(report.get("run_id"), "run_id")
    expected = template(report["host"]["system"], run_id)
    host, build, authorization = report["host"], report["build"], report["authorization"]
    uid = integer(host.get("uid"), "non-root uid", 1)
    text(host.get("os_version"), "OS version")
    text(host.get("architecture"), "architecture")
    require(Path(text(host.get("home"), "home")).is_absolute(), "home must be absolute")
    boot = text(host.get("boot_identity"), "boot identity")
    require(host.get("linger") is False if host["system"] == "Linux"
            else host.get("linger") == "not_applicable", "linger must be off; do not change it")
    require(re.fullmatch(r"[0-9a-f]{40}", str(build.get("revision"))) is not None, "invalid revision")
    require(re.fullmatch(r"[0-9a-f]{64}", str(build.get("binary_sha256"))) is not None, "invalid binary SHA-256")
    text(build.get("version"), "binary version")
    for field in ("isolated_account", "native_changes", "two_logout_login_cycles"):
        require(authorization.get(field) is True, f"missing authorization: {field}")
    text(authorization.get("reference"), "authorization reference")
    artifact(root, report.get("command_transcript"))

    work = Path(text(report.get("fixture_directory"), "fixture directory"))
    require(work.is_absolute() and Path(host["home"]) in work.parents and ".." not in work.parts,
            "fixture directory must be below the disposable account home")
    python = text(report.get("python"), "absolute Python interpreter")
    require(Path(python).is_absolute(), "Python interpreter must be absolute")
    artifact(root, report.get("fixture_source"))
    ids = []
    for mode in MODES:
        fixture = report["fixtures"][mode]
        fixture_id = expected["fixtures"][mode]["id"]
        require(fixture.get("id") == fixture_id, "fixture ID does not belong to this run")
        shown = read_json(artifact(root, fixture.get("show")))
        require(shown.get("schema_version") == "loboot.cli/v1" and shown.get("ok") is True,
                "invalid fixture show envelope")
        manifest = shown["data"]["service"]["manifest"]
        require(manifest.get("id") == fixture_id and manifest.get("mode") == mode
                and manifest.get("restart") == ("never" if mode == "once" else "always"),
                "fixture manifest mode/restart/identity mismatch")
        require(manifest.get("cwd") == str(work) and manifest.get("executable") == python
                and manifest.get("args") == [str(work / "fixture.py"), "--run-id", run_id,
                                              "--mode", mode, "--log", str(work / f"{mode}.jsonl")],
                "manifest is not the scoped invocation-recording fixture")
        ids.append(fixture_id)

    moments, event_sets = {}, {}
    for stage in STAGES:
        observation = report["observations"][stage]
        moments[stage] = timestamp(observation.get("at"))
        event_sets[stage] = {}
        for mode in MODES:
            job = observation["jobs"][mode]
            result = read_json(artifact(root, job.get("status")))
            require(result.get("schema_version") == "loboot.cli/v1" and result.get("ok") is True,
                    "status must be a successful CLI envelope")
            data = result["data"]
            require(data.get("id") == report["fixtures"][mode]["id"] and data.get("installed") is True,
                    "status refers to the wrong job")
            native = data["native"]
            require(native.get("enabled") is (stage in ("armed", "activated")),
                    f"{stage}/{mode}: wrong or unknown enabled state")
            running = stage == "activated" and mode == "service"
            require(native.get("running") is running, f"{stage}/{mode}: wrong or unknown running state")
            if host["system"] == "Darwin":
                require(native.get("loaded") is (stage == "activated"),
                        f"{stage}/{mode}: wrong or unknown launchd loaded state")
            raw = artifact(root, job.get("events"), empty=True)
            require(not raw or raw.endswith(b"\n"), "incomplete event log line")
            events = [read_json(line) for line in raw.splitlines()]
            require(len(events) == (0 if stage == "armed" else 1),
                    f"{stage}/{mode}: unexpected invocation count")
            for event in events:
                require(event.get("schema_version") == "loboot.login-event/v1"
                        and event.get("run_id") == run_id and event.get("mode") == mode
                        and event.get("uid") == uid and type(event.get("uid")) is int,
                        "event identity mismatch")
                integer(event.get("pid"), "event pid", 1)
                require(moments["armed"] < timestamp(event.get("at")) <= moments[stage],
                        "invocation is outside the armed observation window")
            event_sets[stage][mode] = events
    require(all(moments[a] < moments[b] for a, b in zip(STAGES, STAGES[1:])), "stages out of order")
    require((moments["observed"] - moments["returned"]).total_seconds() >= MIN_OBSERVATION_SECONDS,
            "disabled nonactivation observation must last at least 30 seconds")
    for stage in ("disabled", "returned", "observed"):
        require(event_sets[stage] == event_sets["activated"], "event history changed after disabling")

    identities = []
    for name, before, after in (("enabled", "armed", "activated"),
                                ("disabled", "disabled", "returned")):
        boundary = report["boundaries"][name]
        for field in ("real_logout_login", "no_manual_activation", "session_absent_observed"):
            require(boundary.get(field) is True, f"{name}: missing {field}")
        text(boundary.get("observer"), "independent session observer")
        first = text(boundary.get("before_identity"), "before session/manager identity")
        second = text(boundary.get("after_identity"), "after session/manager identity")
        require(first != second, "same session/manager identity cannot prove turnover")
        identities.append((first, second))
        require(boundary.get("boot_identity") == boot, "boot changed: reboot is not logout/login evidence")
        logout, absent, login = (timestamp(boundary.get(key)) for key in ("logout_at", "absent_at", "login_at"))
        require(moments[before] < logout <= absent < login <= moments[after], "invalid session boundary timeline")
        artifact(root, boundary.get("proof"))
        if name == "enabled":
            for mode in MODES:
                require(login <= timestamp(event_sets["activated"][mode][0]["at"]),
                        "fixture ran before the new login began")
    require(identities[0][1] == identities[1][0], "session identity is discontinuous between cycles")
    require(len(set(sum((list(pair) for pair in identities), []))) == 3,
            "session/manager identity was reused")

    cleanup = report["cleanup"]
    require(timestamp(cleanup.get("at")) > moments["observed"], "cleanup must follow final observation")
    for field in ("only_test_owned", "jobs_absent", "processes_exited"):
        require(cleanup.get(field) is True, f"cleanup missing {field}")
    require(cleanup.get("removed_ids") == ids, "cleanup must cover exactly this run's two IDs")
    require(cleanup.get("errors") == [], "cleanup errors remain")
    for scope in ("native", "state"):
        before = read_json(artifact(root, cleanup.get(f"{scope}_before")))
        after = read_json(artifact(root, cleanup.get(f"{scope}_after")))
        require(isinstance(before, dict) and isinstance(before.get("entries"), dict)
                and type(before.get("exists")) is bool and before == after,
                f"{scope} inventory changed after cleanup")
    artifact(root, cleanup.get("proof"))
    retained = [f"com.loboot.{id}" for id in ids] if host["system"] == "Darwin" else []
    require(cleanup.get("retained_disabled_labels") == retained,
            "unexpected retained launchd overrides; never reset the whole domain")
    return {"reported_status": "passed", "consistent": True, "independently_verified": False,
            "reason": "Consistent operator report; review original session proof before claiming acceptance."}


def inventory(directory):
    """Bounded descriptor-relative inventory, preserving links without following them."""
    directory = directory.absolute()
    require(".." not in directory.parts, "traversing inventory directory")
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    entries, consumed = {}, 0
    try:
        for part in directory.parts[1:]:
            try:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                dir_fd=descriptor)
            except FileNotFoundError:
                return {"exists": False, "entries": {}}
            os.close(descriptor)
            descriptor = child

        def bounded_names(parent):
            names = []
            with os.scandir(parent) as scan:
                for entry in scan:
                    require(len(names) + len(entries) < 10000, "inventory exceeds 10000 entries")
                    names.append(entry.name)
            return sorted(names)

        def visit(parent, prefix, depth):
            nonlocal consumed
            require(depth <= 32, "inventory exceeds 32 directory levels")
            for name in bounded_names(parent):
                require(len(entries) < 10000, "inventory exceeds 10000 entries")
                relative = f"{prefix}/{name}" if prefix else name
                info = os.stat(name, dir_fd=parent, follow_symlinks=False)
                if stat.S_ISLNK(info.st_mode):
                    entries[relative] = {"symlink": os.readlink(name, dir_fd=parent)}
                elif stat.S_ISDIR(info.st_mode):
                    child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                                    dir_fd=parent)
                    try:
                        entries[relative] = {"directory": True}
                        visit(child, relative, depth + 1)
                    finally:
                        os.close(child)
                else:
                    child = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK,
                                    dir_fd=parent)
                    try:
                        info = os.fstat(child)
                        require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1
                                and info.st_uid == os.getuid(),
                                "inventory input must be an owned single-link regular file")
                        require(info.st_size <= 4 * 1024 * 1024, "inventory file exceeds 4 MiB")
                        digest, size = hashlib.sha256(), 0
                        while size <= 4 * 1024 * 1024:
                            chunk = os.read(child, min(65536, 4 * 1024 * 1024 + 1 - size))
                            if not chunk:
                                break
                            size += len(chunk)
                            consumed += len(chunk)
                            require(consumed <= 64 * 1024 * 1024, "inventory exceeds 64 MiB")
                            digest.update(chunk)
                        require(size <= 4 * 1024 * 1024, "inventory file exceeds 4 MiB")
                        entries[relative] = {"sha256": digest.hexdigest()}
                    finally:
                        os.close(child)
        visit(descriptor, "", 0)
        return {"exists": True, "entries": entries}
    finally:
        os.close(descriptor)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    new = sub.add_parser("template", help="print an unrun evidence template; no native operations")
    new.add_argument("--system", required=True, choices=("Linux", "Darwin"))
    new.add_argument("--run-id", default=None)
    check = sub.add_parser("validate", help="read local evidence only; never establishes authenticity")
    check.add_argument("report", type=Path)
    listing = sub.add_parser("inventory", help="read-only file inventory for before/after cleanup")
    listing.add_argument("directory", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.command == "template":
            result, code = template(args.system, args.run_id or uuid.uuid4().hex), 0
        elif args.command == "inventory":
            result, code = inventory(args.directory), 0
        else:
            report_path = args.report.absolute()
            result = validate(read_json(read_regular(report_path)), report_path.parent)
            code = 0 if result["consistent"] else 77
    except (ValueError, OSError, KeyError, TypeError, AttributeError, RecursionError) as exc:
        result, code = {"consistent": False, "independently_verified": False, "error": str(exc)}, 1
    print(json.dumps(result, indent=2))
    return code


if __name__ == "__main__":
    sys.exit(main())
