#!/usr/bin/env python3
"""Invocation recorder for the explicitly authorized manual login protocol.

Does not contact native managers or create a log. The operator must first create
an empty private regular log inside the disposable account's private test folder.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import signal
import stat
import sys


def record(log, run_id, mode):
    if not re.fullmatch(r"[0-9a-f]{32}", run_id) or mode not in ("once", "service"):
        raise ValueError("invalid fixture identity")
    log = Path(log)
    if not log.is_absolute() or ".." in log.parts:
        raise ValueError("log must be an absolute non-traversing path")
    # Descriptor-relative traversal refuses symlinks, including parent components.
    directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in log.parts[1:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=directory)
            os.close(directory)
            directory = child
        parent = os.fstat(directory)
        if parent.st_uid != os.getuid() or parent.st_mode & 0o077:
            raise ValueError("log parent must be a private directory owned by the current user")
        descriptor = os.open(log.name, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK,
                             dir_fd=directory)
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_nlink != 1 or info.st_mode & 0o077:
                raise ValueError("log must be a private, owned, single-link regular file")
            event = {"schema_version": "loboot.login-event/v1", "run_id": run_id,
                     "mode": mode, "pid": os.getpid(), "uid": os.getuid(),
                     "at": datetime.now(timezone.utc).isoformat()}
            raw = (json.dumps(event) + "\n").encode()
            if os.write(descriptor, raw) != len(raw):
                raise OSError("partial event write; evidence must be marked failed")
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    finally:
        os.close(directory)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--mode", required=True, choices=("once", "service"))
    parser.add_argument("--log", required=True, type=Path)
    args = parser.parse_args(argv)
    record(args.log, args.run_id, args.mode)
    if args.mode == "service":
        while True:
            signal.pause()
    return 0


if __name__ == "__main__":
    sys.exit(main())
