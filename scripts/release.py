#!/usr/bin/env python3
"""Build and verify local binary release candidates. Never publish anything.

Python 3.11+ and the standard library only. Build the native binary separately
with `cargo build --locked --release` before using the build command.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib


TARGETS = {
    "x86_64-unknown-linux-gnu": "linux",
    "aarch64-unknown-linux-gnu": "linux",
    "x86_64-apple-darwin": "macos",
    "aarch64-apple-darwin": "macos",
}
SCHEMA = "loboot.release/v1"
SOURCE_REPOSITORY = "https://github.com/WhiteKiwi/loboot"
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
LICENSE_NAMES = ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE")
ARCHIVE_FILES = ("loboot", "README.md", "LICENSE-STATUS.txt", "THIRD-PARTY-LICENSES.txt", "RUST-COPYRIGHT.html")
PENDING_LICENSE = (
    "License selection is pending. No license has been assigned to loboot.\n"
    "This archive is a release-readiness candidate, not a published release.\n"
    "Do not infer permission to use, modify, or redistribute this software.\n"
)


class ReleaseError(ValueError):
    """A release input or verification step failed."""


def validate_version(value: str) -> str:
    if not isinstance(value, str) or not VERSION.fullmatch(value):
        raise ReleaseError("version must be a stable X.Y.Z version with no leading zeroes")
    return value


def validate_tag(tag: str | None, version: str) -> None:
    if tag is not None and tag != f"v{version}":
        raise ReleaseError(f"tag must be exactly v{version}")


def validate_target(target: str) -> None:
    if target not in TARGETS:
        raise ReleaseError(f"unsupported target: {target}")


def read_package(repo: Path) -> dict:
    with (repo / "Cargo.toml").open("rb") as source:
        package = tomllib.load(source).get("package", {})
    if not isinstance(package, dict) or package.get("name") != "loboot":
        raise ReleaseError("Cargo.toml package.name must be loboot")
    validate_version(package.get("version"))
    return package


def regular_file(path: Path) -> bytes:
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ReleaseError(f"expected a regular file, not a link: {path}")
    return path.read_bytes()


def chosen_licenses(repo: Path, package: dict) -> list[tuple[str, bytes]]:
    license_id = package.get("license")
    license_file = package.get("license-file")
    if not license_id and not license_file:
        raise ReleaseError("Cargo.toml must explicitly select license or license-file")
    if license_id and license_file:
        raise ReleaseError("choose one Cargo license field: license or license-file")
    if license_id is not None and (not isinstance(license_id, str) or not license_id.strip()):
        raise ReleaseError("Cargo license must be a nonempty string")
    if license_file is not None and license_file not in LICENSE_NAMES:
        raise ReleaseError("Cargo license-file must name LICENSE, LICENSE-MIT, or LICENSE-APACHE")
    names = [license_file] if license_file else [name for name in LICENSE_NAMES if (repo / name).exists()]
    if not names:
        raise ReleaseError("add a chosen LICENSE, LICENSE-MIT, or LICENSE-APACHE file before publishing")
    if license_id == "MIT OR Apache-2.0" and not {"LICENSE-MIT", "LICENSE-APACHE"}.issubset(names):
        raise ReleaseError("dual licensing requires both LICENSE-MIT and LICENSE-APACHE")
    result = []
    for name in names:
        content = regular_file(repo / name)
        if not content.strip():
            raise ReleaseError(f"license file is empty: {name}")
        content.decode("utf-8")
        result.append((name, content))
    return result


def license_status(repo: Path, package: dict) -> bytes:
    if not package.get("license") and not package.get("license-file"):
        return PENDING_LICENSE.encode("utf-8")
    files = chosen_licenses(repo, package)
    selected = package.get("license") or f"see {package['license-file']}"
    lines = [f"License selected in Cargo.toml: {selected}\n"]
    for name, content in files:
        lines.append(f"\n--- {name} ---\n{content.decode('utf-8').rstrip()}\n")
    return "".join(lines).encode("utf-8")


def run(command: list[str], *, cwd: Path | None = None, env: dict | None = None) -> str:
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True, timeout=30, check=False)
    if result.returncode:
        raise ReleaseError(f"{command[0]} failed ({result.returncode}): {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def require_native_target(target: str) -> None:
    validate_target(target)
    hosts = re.findall(r"^host: (\S+)$", run(["rustc", "-vV"]), re.MULTILINE)
    if hosts != [target]:
        raise ReleaseError(f"rustc host {hosts!r} must match selected native target {target}; no cross-linking")


def check_version(binary: Path, version: str, **kwargs) -> None:
    actual = run([str(binary), "--version"], **kwargs).strip()
    if actual != f"loboot {version}":
        raise ReleaseError(f"binary version mismatch: expected 'loboot {version}', got {actual!r}")


def require_binary_header(header: bytes, target: str) -> None:
    """Check the actual architecture, including when Rosetta can run x86 binaries."""
    validate_target(target)
    if target.endswith("unknown-linux-gnu"):
        valid = (
            len(header) >= 64 and header[:7] == b"\x7fELF\x02\x01\x01"
            and int.from_bytes(header[16:18], "little") in (2, 3)  # executable or PIE
            and int.from_bytes(header[18:20], "little") == (62 if target.startswith("x86_64") else 183)  # ELF machine
        )
    else:
        valid = (
            len(header) >= 32 and header[:4] == b"\xcf\xfa\xed\xfe"  # MH_MAGIC_64
            and int.from_bytes(header[4:8], "little") == (0x01000007 if target.startswith("x86_64") else 0x0100000C)
            and int.from_bytes(header[12:16], "little") == 2  # MH_EXECUTE
        )
    if not valid:
        raise ReleaseError(f"binary header does not match native executable target {target}")


def package_readme(version: str, target: str, tag: str | None) -> bytes:
    """Ship concise installation guidance without broken checkout-relative links."""
    source = f"{SOURCE_REPOSITORY}/blob/{tag or 'main'}"
    return f"""# loboot {version} ({target})

Native user services, explicit plans, no daemon.

This archive contains a native binary, this README, LICENSE-STATUS.txt,
THIRD-PARTY-LICENSES.txt, and RUST-COPYRIGHT.html.
Read LICENSE-STATUS.txt before using or redistributing it. A pending license
means this is an unpublished readiness candidate, with no license granted.

## Manual installation

After verifying the supplied SHA256SUMS.txt and checking the license, extract
the archive and run `./loboot --version` and `./loboot --help` from this directory.
To install for your account, create `$HOME/.local/bin`, copy `loboot` there with
executable permissions, and add that directory to PATH. Replacing a binary does
not update stored service definitions. Installation never registers, starts,
or enables a service automatically.

## Constraints and safe inspection

Only the target named above is supported by this archive. Linux live operations
require systemd 240+ and a reachable user manager. macOS live operations require
a graphical login session. These candidates do not claim macOS signing or
notarization. Real login-session lifecycle acceptance remains pending.

`--root ABSOLUTE_PATH --platform linux` (or `macos`) selects an offline sandbox;
runtime actions are refused there. `add ... --dry-run` plans without creating
the root. Executable and working-directory paths must be absolute, canonical,
and non-symlinked. Arguments are literal; no shell is implied. Bounded log reads do not cap active
log-file growth; inspect retention and privacy guidance before sharing output. No
secrets, system/boot services, Windows support, or automatic recovery is provided.

## Source documentation

- [Installation and uninstalling]({source}/docs/INSTALL.md)
- [Operator guide]({source}/docs/OPERATOR.md)
- [CLI contract]({source}/docs/CLI.md)
- [Troubleshooting and recovery]({source}/docs/TROUBLESHOOTING.md)
- [Release process]({source}/docs/RELEASE.md)
- [Source repository]({SOURCE_REPOSITORY})

Documentation links use {'the matching tag' if tag else 'the main branch; a candidate may differ from main'}.
""".encode("utf-8")


def smoke(binary: Path, version: str, target: str) -> None:
    """Only inspect CLI output and an offline plan. Never invoke a service manager."""
    validate_target(target)
    binary = binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="loboot-smoke-") as temporary:
        cwd = Path(temporary).resolve(strict=True)  # /var is a symlink on macOS.
        root = cwd / "offline-root"
        # PATH is intentionally empty, so unexpected service-manager lookup fails.
        env = dict(os.environ, PATH="", HOME=str(cwd))
        check_version(binary, version, cwd=cwd, env=env)
        help_text = run([str(binary), "--help"], cwd=cwd, env=env)
        if "Usage:" not in help_text or "loboot" not in help_text:
            raise ReleaseError("--help did not describe the loboot CLI")
        output = run([
            str(binary), "--json", "--root", str(root), "--platform", TARGETS[target],
            "add", "release-smoke", "--cwd", str(cwd), "--mode", "once", "--dry-run",
            "--", str(binary), "--version",
        ], cwd=cwd, env=env)
        response = json.loads(output)
        if not isinstance(response, dict) or response.get("schema_version") != "loboot.cli/v1" or response.get("ok") is not True:
            raise ReleaseError("offline add --dry-run did not return a successful CLI envelope")
        data = response.get("data")
        if not isinstance(data, dict) or data.get("dry_run") is not True or data.get("native_commands") != []:
            raise ReleaseError("offline add --dry-run did not return a native-command-free plan")
        if root.exists() or root.is_symlink():
            raise ReleaseError("offline add --dry-run unexpectedly created its root")


def archive_stem(version: str, target: str) -> str:
    validate_version(version)
    validate_target(target)
    return f"loboot-v{version}-{target}"


def write_archive(path: Path, stem: str, contents: dict[str, bytes]) -> None:
    # An empty gzip filename and zero timestamps prevent runner paths/times leaking
    # into the artifact. Tar ownership and permissions are fixed as well.
    with path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                directory = tarfile.TarInfo(stem)
                directory.type = tarfile.DIRTYPE
                directory.mode = 0o755
                archive.addfile(directory)
                for name in ARCHIVE_FILES:
                    payload = contents[name]
                    member = tarfile.TarInfo(f"{stem}/{name}")
                    member.size = len(payload)
                    member.mode = 0o755 if name == "loboot" else 0o644
                    archive.addfile(member, io.BytesIO(payload))


def verify_archive(path: Path, version: str, target: str, *, execute: bool) -> None:
    """Inspect every member before extracting explicit regular files ourselves."""
    stem = archive_stem(version, target)
    expected = {stem: 0o755, **{f"{stem}/{name}": (0o755 if name == "loboot" else 0o644) for name in ARCHIVE_FILES}}
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        if len(members) != len(expected) or {m.name for m in members} != set(expected):
            raise ReleaseError("archive inventory must contain exactly its root and the declared binary, README, and license files")
        for member in members:
            if (member.name == stem and not member.isdir()) or (member.name != stem and not member.isreg()):
                raise ReleaseError(f"archive links or special files are forbidden: {member.name}")
            if member.mode != expected[member.name]:
                raise ReleaseError(f"incorrect archive permissions: {member.name}")
            if (member.uid, member.gid, member.uname, member.gname, member.mtime) != (0, 0, "", "", 0):
                raise ReleaseError(f"non-deterministic archive metadata: {member.name}")
            if member.name != stem and member.size == 0:
                raise ReleaseError(f"empty archive member: {member.name}")
        binary_source = archive.extractfile(f"{stem}/loboot")
        if binary_source is None:
            raise ReleaseError("cannot read archived binary")
        with binary_source:
            require_binary_header(binary_source.read(64), target)
        if execute:
            with tempfile.TemporaryDirectory(prefix="loboot-extract-") as temporary:
                destination = Path(temporary).resolve(strict=True) / stem
                destination.mkdir()
                for member in members:
                    if member.name == stem:
                        continue
                    # No extractall: exact inventory + regular files prevent traversal,
                    # hardlinks, symlinks, and device nodes regardless of Python version.
                    target_path = destination / member.name.split("/")[1]
                    source = archive.extractfile(member)
                    if source is None:
                        raise ReleaseError(f"cannot read archive member: {member.name}")
                    with source, target_path.open("xb") as output:
                        shutil.copyfileobj(source, output)
                    target_path.chmod(member.mode)
                smoke(destination / "loboot", version, target)


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def dump_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def bundle_metadata(version: str, target: str, archive: Path) -> dict:
    return {"schema_version": SCHEMA, "version": version, "target": target,
            "archive": archive.name, "sha256": digest(archive)}


def verify_bundle(directory: Path, version: str, target: str, *, execute: bool) -> dict:
    archive = directory / f"{archive_stem(version, target)}.tar.gz"
    expected = {archive.name, "manifest.json", "SHA256SUMS.txt"}
    if {p.name for p in directory.iterdir()} != expected:
        raise ReleaseError(f"bundle has missing or unexpected files: {directory}")
    for filename in expected:
        regular_file(directory / filename)
    metadata = bundle_metadata(version, target, archive)
    if json.loads((directory / "manifest.json").read_text(encoding="utf-8")) != metadata:
        raise ReleaseError(f"manifest does not match archive, version, target, or checksum: {directory}")
    checksum = f"{metadata['sha256']}  {archive.name}\n"
    if (directory / "SHA256SUMS.txt").read_text(encoding="utf-8") != checksum:
        raise ReleaseError(f"SHA256SUMS.txt must contain the matching bare archive filename: {directory}")
    verify_archive(archive, version, target, execute=execute)
    return metadata


def require_empty_output(output: Path) -> None:
    if output.exists() and (not output.is_dir() or any(output.iterdir())):
        raise ReleaseError(f"output must be absent or an empty directory: {output}")


def dependency_packages(metadata: dict) -> list[dict]:
    """Select packages reachable through Cargo's target-filtered resolve graph."""
    resolve = metadata.get("resolve") or {}
    root = resolve.get("root")
    nodes = {node["id"]: node for node in resolve.get("nodes", [])}
    packages = {package["id"]: package for package in metadata.get("packages", [])}
    if root not in nodes or root not in packages:
        raise ReleaseError("Cargo metadata has no root resolve node")
    reachable, pending = set(), [root]
    while pending:
        identifier = pending.pop()
        if identifier in reachable:
            continue
        if identifier not in nodes or identifier not in packages:
            raise ReleaseError("Cargo resolve graph references a missing package")
        reachable.add(identifier)
        pending.extend(nodes[identifier]["dependencies"])
    return sorted((packages[identifier] for identifier in reachable),
                  key=lambda package: (package["name"], package["version"]))


def third_party_notices(repo: Path, target: str) -> dict[str, bytes]:
    """Include locked target dependencies and their upstream notice texts.

    Includes development/build dependencies conservatively. No license is inferred
    from a crate's name, and unknown license choices fail closed for review.
    """
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                               "--filter-platform", target], cwd=repo))
    parts = ["Third-party components from the locked Cargo dependency graph.\n"
             "Development/build dependencies are included conservatively.\n"
             "MIT is selected wherever offered; Unicode-3.0 also applies to unicode-ident.\n"]
    for package in dependency_packages(metadata):
        if package["name"] == "loboot" and package.get("source") is None:
            continue
        expression = package.get("license") or ""
        normalized = expression.replace("/", " OR ")
        accepted = {"MIT", "MIT OR Apache-2.0", "Apache-2.0 OR MIT", "Unlicense OR MIT",
                    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
                    "(MIT OR Apache-2.0) AND Unicode-3.0"}
        if normalized not in accepted:
            raise ReleaseError(f"review dependency license before release: {package['name']} {expression!r}")
        root = Path(package["manifest_path"]).parent
        notices = sorted(path for path in root.rglob("*") if path.is_file()
                         and path.name.upper().startswith(("LICENSE", "COPYING", "NOTICE")))
        if not notices:
            raise ReleaseError(f"dependency has no bundled license texts: {package['name']}")
        parts.append(f"\n{'=' * 72}\n{package['name']} {package['version']}\nLicense: {expression}\n")
        for notice in notices:
            parts.append(f"\n--- {notice.relative_to(root)} ---\n{regular_file(notice).decode('utf-8').rstrip()}\n")
    sysroot = Path(run(["rustc", "--print", "sysroot"]).strip())
    copyright_file = sysroot / "share/doc/rust/COPYRIGHT-library.html"
    runtime = regular_file(copyright_file)
    if not runtime.strip():
        raise ReleaseError("Rust runtime copyright notice is empty")
    return {"THIRD-PARTY-LICENSES.txt": "".join(parts).encode("utf-8"), "RUST-COPYRIGHT.html": runtime}


def build(repo: Path, target: str, output: Path, tag: str | None = None) -> dict:
    package = read_package(repo)
    version = package["version"]
    validate_tag(tag, version)
    require_native_target(target)
    require_empty_output(output)
    binary = repo / "target" / "release" / "loboot"
    payload = regular_file(binary)
    require_binary_header(payload[:64], target)
    if not os.access(binary, os.X_OK):
        raise ReleaseError("target/release/loboot must be executable")
    check_version(binary.resolve(strict=True), version)
    contents = {"loboot": payload, "README.md": package_readme(version, target, tag),
                "LICENSE-STATUS.txt": license_status(repo, package),
                **third_party_notices(repo, target)}
    with tempfile.TemporaryDirectory(prefix="loboot-package-") as temporary:
        staging = Path(temporary)
        archive = staging / f"{archive_stem(version, target)}.tar.gz"
        write_archive(archive, archive_stem(version, target), contents)
        metadata = bundle_metadata(version, target, archive)
        dump_json(staging / "manifest.json", metadata)
        (staging / "SHA256SUMS.txt").write_text(f"{metadata['sha256']}  {archive.name}\n", encoding="utf-8")
        verify_bundle(staging, version, target, execute=True)
        output.mkdir(parents=True, exist_ok=True)
        for artifact in staging.iterdir():
            shutil.copyfile(artifact, output / artifact.name)
    return verify_bundle(output, version, target, execute=False)


def check(repo: Path, target: str, directory: Path, tag: str | None = None) -> dict:
    version = read_package(repo)["version"]
    validate_tag(tag, version)
    require_native_target(target)
    return verify_bundle(directory, version, target, execute=True)


def release_notes(repo: Path, tag: str) -> str:
    """Use one nonempty curated section from the exact tagged checkout."""
    version = read_package(repo)["version"]
    validate_tag(tag, version)
    changelog = regular_file(repo / "CHANGELOG.md").decode("utf-8")
    heading = rf"^##[ \t]+(?:\[{re.escape(version)}\]|{re.escape(version)})(?:[ \t]+[^\n]*)?$"
    matches = list(re.finditer(heading, changelog, re.MULTILINE))
    if len(matches) != 1:
        raise ReleaseError(f"CHANGELOG.md must contain exactly one level-two section for {version}")
    remaining = changelog[matches[0].end():]
    following = re.search(r"^##[ \t]+", remaining, re.MULTILINE)
    body = remaining[:following.start()] if following else remaining
    body = body.strip()
    if not body:
        raise ReleaseError(f"CHANGELOG.md section for {version} must contain nonempty curated release notes")
    return body + "\n"


def gate(repo: Path, tag: str) -> dict:
    package = read_package(repo)
    version = package["version"]
    validate_tag(tag, version)
    licenses = chosen_licenses(repo, package)
    release_notes(repo, tag)
    # Cargo publish=false intentionally remains valid: binary releases and
    # publication to crates.io are separate decisions.
    return {"version": version, "tag": tag, "license_files": [name for name, _ in licenses], "ready": True}


def collect(repo: Path, artifacts: Path, output: Path, version: str) -> dict:
    validate_version(version)
    if read_package(repo)["version"] != version:
        raise ReleaseError("collection version must match Cargo.toml")
    require_empty_output(output)
    if not artifacts.is_dir() or artifacts.is_symlink():
        raise ReleaseError("artifacts must be a directory, not a link")
    files = set()
    for path in artifacts.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise ReleaseError(f"unexpected artifact link or special file: {path}")
        if path.is_file():
            files.add(path)
    bundles = []
    expected_files = set()
    for target in sorted(TARGETS):
        name = f"{archive_stem(version, target)}.tar.gz"
        matches = sorted(path for path in files if path.name == name)
        if len(matches) != 1:
            raise ReleaseError(f"expected exactly one archive for {target}, found {len(matches)}")
        archive = matches[0]
        bundles.append((archive, verify_bundle(archive.parent, version, target, execute=False)))
        expected_files.update(archive.parent / name for name in (archive.name, "manifest.json", "SHA256SUMS.txt"))
    if files != expected_files:
        raise ReleaseError("downloaded artifacts contain unexpected files or archives")
    metadata = {"schema_version": SCHEMA, "version": version, "artifacts": [entry for _, entry in bundles]}
    output.mkdir(parents=True, exist_ok=True)
    for archive, entry in bundles:
        destination = output / archive.name
        shutil.copyfile(archive, destination)
        if digest(destination) != entry["sha256"]:
            raise ReleaseError(f"copied archive checksum changed: {archive.name}")
    (output / "SHA256SUMS.txt").write_text(
        "".join(f"{entry['sha256']}  {archive.name}\n" for archive, entry in bundles), encoding="utf-8")
    dump_json(output / "manifest.json", metadata)
    return metadata


def parser() -> argparse.ArgumentParser:
    cli = argparse.ArgumentParser(description=__doc__)
    commands = cli.add_subparsers(dest="command", required=True)
    for command in ("build", "check", "gate", "notes", "collect"):
        sub = commands.add_parser(command)
        sub.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
        if command in ("build", "check"):
            sub.add_argument("--target", choices=sorted(TARGETS), required=True)
            sub.add_argument("--tag")
            sub.add_argument("--output" if command == "build" else "--input", type=Path, required=True)
        elif command in ("gate", "notes"):
            sub.add_argument("--tag", required=True)
            if command == "notes":
                sub.add_argument("--output", type=Path, required=True)
        else:
            sub.add_argument("--artifacts", type=Path, required=True)
            sub.add_argument("--out", type=Path, required=True)
            sub.add_argument("--version", required=True)
    return cli


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        repo = args.repo.resolve(strict=True)
        if args.command == "build":
            result = build(repo, args.target, args.output, args.tag)
        elif args.command == "check":
            result = check(repo, args.target, args.input, args.tag)
        elif args.command == "gate":
            result = gate(repo, args.tag)
        elif args.command == "notes":
            notes = release_notes(repo, args.tag)
            with args.output.open("x", encoding="utf-8") as destination:
                destination.write(notes)
            result = {"tag": args.tag, "notes": str(args.output)}
        else:
            result = collect(repo, args.artifacts, args.out, args.version)
        print(json.dumps(result, sort_keys=True))
        return 0
    except (ReleaseError, OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f"release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
