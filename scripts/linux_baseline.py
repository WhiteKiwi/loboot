#!/usr/bin/env python3
"""Verify an extracted Linux candidate on its native Ubuntu 22.04 baseline.

Python 3.11+, GNU readelf, and the pinned native Rust toolchain are required.
Evidence is separate from the immutable loboot.release/v1 bundle. No services,
containers, emulators, installations, or publication are invoked here.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import tarfile
import tempfile

import release


CEILING = "2.35"
RUST = "1.99.0"
TARGETS = {
    "x86_64-unknown-linux-gnu": ("ubuntu-22.04", "x86_64", "X64"),
    "aarch64-unknown-linux-gnu": ("ubuntu-22.04-arm", "aarch64", "ARM64"),
}
NUMBER = r"(?:0|[1-9][0-9]*)"
VERSION = re.compile(rf"{NUMBER}\.{NUMBER}(?:\.{NUMBER})?")
OFFSET = r"(?:0x)?[0-9a-fA-F]+:"


class BaselineError(ValueError):
    """Missing, unexpected, or incompatible baseline evidence."""


def numeric_version(value: str) -> tuple[int, int, int]:
    if not VERSION.fullmatch(value):
        raise BaselineError(f"malformed numeric ABI version: {value!r}")
    parts = tuple(int(part) for part in value.split("."))
    return parts if len(parts) == 3 else (*parts, 0)


def version_name(name: str) -> None:
    # Only namespaces already used by this Rust executable are understood.
    # In particular GLIBC_PRIVATE, GLIBC_ABI_*, and GLIBCXX need explicit review.
    if not re.fullmatch(rf"(?:GLIBC|GCC)_{VERSION.pattern}", name):
        raise BaselineError(f"unexpected ABI version name: {name!r}")


def parse_version_symbols(text: str, needs: list[dict]) -> list[int]:
    prefix = text.split("Version needs section", 1)[0]
    lines = [line.strip() for line in prefix.splitlines() if line.strip()]
    header = re.fullmatch(r"Version symbols section '\.gnu\.version' contains ([1-9][0-9]*) entries:", lines.pop(0)) if lines else None
    if not header or not lines or not re.fullmatch(r"Addr: 0x[0-9a-fA-F]+\s+Offset: 0x[0-9a-fA-F]+\s+Link: [0-9]+ \(\.dynsym\)", lines.pop(0)):
        raise BaselineError("missing or malformed version-symbol header")
    by_index = {entry["index"]: entry["name"] for entry in needs}
    by_index.update({0: "*local*", 1: "*global*"})
    indexes = []
    for line in lines:
        row = re.fullmatch(r"([0-9a-fA-F]+):\s+(.+)", line)
        if not row or int(row.group(1), 16) != len(indexes):
            raise BaselineError("malformed version-symbol row offset")
        entries = row.group(2)
        matches = list(re.finditer(r"([0-9a-fA-F]+)h?\s+\(([^()]+)\)", entries))
        end = 0
        if not 1 <= len(matches) <= 4:
            raise BaselineError("malformed version-symbol row")
        for match in matches:
            index, name = int(match.group(1), 16), match.group(2)
            if entries[end:match.start()].strip() or by_index.get(index) != name:
                raise BaselineError("version symbols disagree with version needs")
            indexes.append(index)
            end = match.end()
        if entries[end:].strip():
            raise BaselineError("malformed version-symbol trailing data")
    if len(indexes) != int(header.group(1)):
        raise BaselineError("truncated version-symbol table")
    return indexes


def parse_version_needs(text: str) -> list[dict]:
    headers = list(re.finditer(r"^Version needs section '\.gnu\.version_r' contains ([1-9][0-9]*) entr(?:y|ies):$", text, re.M))
    if len(headers) != 1 or not re.search(r"^Version symbols section '\.gnu\.version' contains [1-9][0-9]* entries:$", text, re.M):
        raise BaselineError("missing or malformed GNU version sections")
    if "Version definition section" in text:
        raise BaselineError("unexpected version definitions in executable")
    entries, files, current, remaining = [], [], None, 0
    lines = [line.strip() for line in text[headers[0].end():].splitlines() if line.strip()]
    if not lines or not re.fullmatch(r"Addr: 0x[0-9a-fA-F]+\s+Offset: 0x[0-9a-fA-F]+\s+Link: [0-9]+ \(\.dynstr\)", lines.pop(0)):
        raise BaselineError("missing or malformed version-needs address header")
    for line in lines:
        file_match = re.fullmatch(rf"{OFFSET}\s+Version: 1\s+File: (\S+)\s+Cnt: ([1-9][0-9]*)", line)
        name_match = re.fullmatch(rf"{OFFSET}\s+Name: (\S+)\s+Flags: (none|WEAK)\s+Version: ([0-9]+)", line)
        if file_match:
            if remaining:
                raise BaselineError("truncated version-needs entries")
            current, count = file_match.groups()
            if current in files:
                raise BaselineError("duplicate version-needs library")
            files.append(current)
            remaining = int(count)
        elif name_match and current and remaining:
            name, flags, index = name_match.groups()
            version_name(name)
            if int(index) < 2 or any(entry["index"] == int(index) for entry in entries):
                raise BaselineError("duplicate or invalid version index")
            entries.append({"library": current, "name": name, "flags": flags, "index": int(index)})
            remaining -= 1
        else:
            raise BaselineError(f"malformed version-needs line: {line!r}")
    if remaining or len(files) != int(headers[0].group(1)):
        raise BaselineError("truncated or inconsistent version-needs inventory")
    glibc = {entry["name"] for entry in entries if entry["name"].startswith("GLIBC_")}
    if not glibc or glibc != set(re.findall(r"\bGLIBC[^\s()]+", text)):
        raise BaselineError("missing or inconsistent GLIBC references")
    parse_version_symbols(text, entries)
    return entries


def parse_symbols(text: str, needs: list[dict], version_indexes: list[int] | None = None) -> list[dict]:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    header = re.fullmatch(r"Symbol table '\.dynsym' contains ([1-9][0-9]*) entries:", lines.pop(0)) if lines else None
    if not header or not lines or lines.pop(0).split() != ["Num:", "Value", "Size", "Type", "Bind", "Vis", "Ndx", "Name"]:
        raise BaselineError("missing or malformed dynamic symbol table")
    if len(lines) != int(header.group(1)):
        raise BaselineError("truncated dynamic symbol table")
    if version_indexes is not None and len(version_indexes) != len(lines):
        raise BaselineError("version and dynamic symbol counts disagree")
    by_index = {entry["index"]: entry for entry in needs}
    symbols = []
    for index, line in enumerate(lines):
        match = re.fullmatch(r"([0-9]+):\s+[0-9a-fA-F]+\s+[0-9]+\s+(NOTYPE|OBJECT|FUNC|SECTION|FILE|COMMON|TLS|IFUNC)\s+(LOCAL|GLOBAL|WEAK|UNIQUE)\s+(DEFAULT|INTERNAL|HIDDEN|PROTECTED)\s+(\S+)(?:\s+(.*))?", line)
        if not match or int(match.group(1)) != index:
            raise BaselineError("malformed dynamic symbol entry")
        _, _, binding, _, section, name = match.groups()
        if name and "@" in name:
            versioned = re.fullmatch(r"([^@\s]+)@([^@\s]+) \(([0-9]+)\)", name)
            if not versioned or not (section == "UND" or section.isdecimal()):
                raise BaselineError(f"unexpected versioned dynamic symbol: {name!r}")
            symbol, version, need_index = versioned.groups()
            version_name(version)
            if version_indexes is not None and version_indexes[index] != int(need_index):
                raise BaselineError("version and dynamic symbol indexes disagree")
            need = by_index.get(int(need_index))
            if not need or need["name"] != version or binding not in ("GLOBAL", "WEAK"):
                raise BaselineError("dynamic symbol disagrees with version needs")
            symbols.append({"symbol": symbol, "version": version, "binding": binding, "section": section, "library": need["library"]})
        elif version_indexes is not None and version_indexes[index] not in (0, 1):
            raise BaselineError("unversioned dynamic symbol has a version index")
        elif name and "GLIBC" in name:
            raise BaselineError("malformed GLIBC dynamic symbol")
    if {entry["name"] for entry in needs} != {item["version"] for item in symbols}:
        raise BaselineError("dynamic symbols and version-needs inventory disagree")
    return symbols


def parse_dynamic(text: str, needs: list[dict]) -> list[str]:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    header = re.fullmatch(r"Dynamic section at offset 0x[0-9a-fA-F]+ contains ([1-9][0-9]*) entries:", lines.pop(0)) if lines else None
    if not header or not lines or lines.pop(0).split() != ["Tag", "Type", "Name/Value"]:
        raise BaselineError("missing or malformed dynamic section")
    if len(lines) != int(header.group(1)) or not all(re.fullmatch(r"0x[0-9a-fA-F]+\s+\([A-Z0-9_]+\)\s+.+", line) for line in lines):
        raise BaselineError("truncated or malformed dynamic section")
    needed_lines = [line for line in text.splitlines() if "(NEEDED)" in line]
    libraries = []
    for line in needed_lines:
        match = re.fullmatch(r"\s*0x[0-9a-fA-F]+\s+\(NEEDED\)\s+Shared library: \[([^\[\]/\s]+)\]\s*", line)
        if not match:
            raise BaselineError("malformed dynamic library requirement")
        libraries.append(match.group(1))
    if "libc.so.6" not in libraries or len(libraries) != len(set(libraries)):
        raise BaselineError("missing libc or duplicate dynamic library requirement")
    if not {entry["library"] for entry in needs}.issubset(libraries):
        raise BaselineError("version needs refer to an unlisted dynamic library")
    # No bundled libraries, search-path overrides, or newer packed-relocation ABI.
    if re.search(r"\((?:RPATH|RUNPATH|RELR|RELRSZ|RELRENT)\)", text):
        raise BaselineError("unexpected dynamic search path or packed relocation requirement")
    return libraries


def check_ceiling(needs: list[dict], ceiling: str = CEILING) -> str:
    limit = numeric_version(ceiling)
    versions = [entry["name"][6:] for entry in needs if entry["name"].startswith("GLIBC_")]
    if not versions:
        raise BaselineError("no GLIBC references")
    maximum = max(versions, key=numeric_version)
    if numeric_version(maximum) > limit:
        raise BaselineError(f"GLIBC_{maximum} exceeds GLIBC_{ceiling} ceiling (including weak references)")
    return maximum


def command(argv: list[str], evidence: dict | None = None) -> str:
    record = {"argv": argv}
    if evidence is not None:
        evidence.setdefault("commands", []).append(record)
    try:
        result = subprocess.run(argv, text=True, capture_output=True, timeout=30, check=False,
                                env=dict(os.environ, LC_ALL="C"))
    except (OSError, subprocess.SubprocessError) as error:
        record["error"] = str(error)
        raise
    record.update(stdout=result.stdout, stderr=result.stderr, returncode=result.returncode)
    if result.returncode or result.stderr.strip() or not result.stdout.strip():
        raise BaselineError(f"{argv[0]} failed or returned missing/diagnostic output: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def inspect_binary(binary: Path, target: str, evidence: dict) -> None:
    release.require_binary_header(release.regular_file(binary)[:64], target)
    version = command(["readelf", "--version"], evidence)
    evidence["readelf_version"] = version
    if not version.startswith("GNU readelf "):
        raise BaselineError("GNU readelf is required")
    raw = evidence["readelf"] = {}
    for option, key in (("--version-info", "version_info"), ("--dyn-syms", "dynamic_symbols"), ("--dynamic", "dynamic")):
        raw[key] = command(["readelf", option, "--wide", str(binary)], evidence)
    needs = evidence["version_needs"] = parse_version_needs(raw["version_info"])
    evidence["versioned_symbols"] = parse_symbols(raw["dynamic_symbols"], needs, parse_version_symbols(raw["version_info"], needs))
    evidence["needed_libraries"] = parse_dynamic(raw["dynamic"], needs)
    evidence["max_glibc_reference"] = max((entry["name"][6:] for entry in needs if entry["name"].startswith("GLIBC_")), key=numeric_version)
    check_ceiling(needs)


def environment_evidence(target: str, runner_label: str, evidence: dict) -> None:
    label, machine, runner_arch = TARGETS[target]
    environment = evidence["environment"] = {
        "runner_label": runner_label, "system": platform.system(), "machine": platform.machine(),
        "python": sys.version,
        "os_release": platform.freedesktop_os_release(),
        "runner": {key: os.environ.get(key) for key in ("ImageOS", "ImageVersion", "RUNNER_OS", "RUNNER_ARCH", "RUNNER_ENVIRONMENT")},
        "github": {key: os.environ.get(key) for key in ("GITHUB_REPOSITORY", "GITHUB_SHA", "GITHUB_REF", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT")},
    }
    for name, argv in (("glibc", ["getconf", "GNU_LIBC_VERSION"]), ("rustc", ["rustc", "-vV"]),
                       ("cargo", ["cargo", "--version"]), ("host_cc", ["cc", "--version"]),
                       ("host_ld", ["ld", "--version"]), ("kernel", ["uname", "-srvm"]),
                       ("source_commit", ["git", "rev-parse", "HEAD"])):
        environment[name] = command(argv, evidence).strip()
    if runner_label != label or environment["system"] != "Linux" or environment["machine"] != machine:
        raise BaselineError("runner/host architecture does not match native baseline target")
    if (environment["os_release"].get("ID"), environment["os_release"].get("VERSION_ID")) != ("ubuntu", "22.04"):
        raise BaselineError("native baseline validation requires Ubuntu 22.04")
    if environment["glibc"] != f"glibc {CEILING}":
        raise BaselineError(f"native baseline validation requires runtime glibc {CEILING}")
    if not environment["rustc"].startswith(f"rustc {RUST} ") or re.findall(r"^host: (\S+)$", environment["rustc"], re.M) != [target]:
        raise BaselineError(f"native Rust {RUST} host toolchain is required")
    if not environment["cargo"].startswith(f"cargo {RUST} "):
        raise BaselineError(f"Cargo {RUST} is required")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        if (environment["runner"]["RUNNER_OS"], environment["runner"]["RUNNER_ARCH"]) != ("Linux", runner_arch) or not environment["runner"]["ImageVersion"]:
            raise BaselineError("GitHub runner architecture or ImageVersion evidence is missing")


def check_bundle(directory: Path, target: str, evidence: dict) -> None:
    metadata = json.loads(release.regular_file(directory / "manifest.json"))
    if not isinstance(metadata, dict):
        raise BaselineError("bundle manifest must be a JSON object")
    version = metadata.get("version")
    evidence["bundle"] = release.verify_bundle(directory, version, target, execute=False)
    archive = directory / evidence["bundle"]["archive"]
    # verify_bundle checks exact safe inventory first; never call extractall.
    with tempfile.TemporaryDirectory(prefix="loboot-linux-baseline-") as temporary:
        binary = Path(temporary).resolve() / "loboot"
        with tarfile.open(archive, "r:gz") as source:
            member = source.extractfile(f"{release.archive_stem(version, target)}/loboot")
            if member is None:
                raise BaselineError("missing archived binary")
            with member:
                binary.write_bytes(member.read())
        binary.chmod(0o755)
        evidence["binary_sha256"] = release.digest(binary)
        inspect_binary(binary, target, evidence)
        # Executed only after ABI/architecture validation, on the actual baseline.
        evidence["extracted_archive_offline_smoke"] = "failed"
        release.smoke(binary, version, target)
        evidence["extracted_archive_offline_smoke"] = "passed"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--target", choices=sorted(TARGETS), required=True)
    parser.add_argument("--runner-label", required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args(argv)
    evidence = {"schema_version": "loboot.linux-baseline/v1", "target": args.target,
                "glibc_reference_ceiling": CEILING, "status": "failed",
                "extracted_archive_offline_smoke": "not-run"}
    try:
        if args.report.resolve().is_relative_to(args.input.resolve()):
            raise BaselineError("evidence report must be outside the strict release bundle")
        if args.report.exists() or args.report.is_symlink():
            raise BaselineError("evidence report must not already exist")
        try:
            environment_evidence(args.target, args.runner_label, evidence)
            check_bundle(args.input, args.target, evidence)
            evidence["status"] = "passed"
        except (BaselineError, release.ReleaseError, OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
            evidence["error"] = str(error)
        with args.report.open("x", encoding="utf-8") as report:
            json.dump(evidence, report, indent=2, sort_keys=True)
            report.write("\n")
        if evidence["status"] != "passed":
            raise BaselineError(evidence["error"])
        print(f"Native {args.target} archive passed glibc {CEILING} baseline and offline smoke; evidence: {args.report}")
        return 0
    except (BaselineError, OSError) as error:
        print(f"linux-baseline: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
