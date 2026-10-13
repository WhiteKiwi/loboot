#!/usr/bin/env python3
"""Offline tests for release.py; no Rust installation or service manager needed."""

from __future__ import annotations

from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("loboot_release", Path(__file__).with_name("release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)
LINUX = "x86_64-unknown-linux-gnu"
MACOS = "aarch64-apple-darwin"


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="loboot-release-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.cargo()
        (self.repo / "README.md").write_text("# loboot\nOffline fixture.\n", encoding="utf-8")
        (self.repo / "CHANGELOG.md").write_text("# Changelog\n\n## [0.1.0] - 2026-10-13\n\nInitial version.\n", encoding="utf-8")
        self.binary = self.repo / "target" / "release" / "loboot"
        self.binary.parent.mkdir(parents=True)
        self.fake_binary()
        # CLI smoke uses an explicit script double. Real architecture validation is
        # exercised separately with header fixtures, never inferred from scripts.
        self.header_validator = release.require_binary_header
        header_patch = patch.object(release, "require_binary_header")
        self.header_mock = header_patch.start()
        self.addCleanup(header_patch.stop)
        notices_patch = patch.object(release, "third_party_notices", return_value={"THIRD-PARTY-LICENSES.txt": b"Fixture notice", "RUST-COPYRIGHT.html": b"<html>Fixture runtime notice</html>"})
        notices_patch.start()
        self.addCleanup(notices_patch.stop)

    def cargo(self, extra="", version="0.1.0"):
        (self.repo / "Cargo.toml").write_text(
            f'[package]\nname = "loboot"\nversion = {json.dumps(version)}\npublish = false\n{extra}', encoding="utf-8")

    def fake_binary(self, *, version="0.1.0", creates_root=False, native_commands=None):
        self.binary.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "if args == ['--version']:\n"
            f"    print('loboot {version}')\n"
            "elif args == ['--help']:\n"
            "    print('Usage: loboot [OPTIONS] <COMMAND>')\n"
            "else:\n"
            "    assert os.environ['PATH'] == ''\n"
            "    assert '--root' in args and '--platform' in args\n"
            "    assert args[args.index('--platform') + 1] in ('linux', 'macos')\n"
            "    assert 'add' in args and '--dry-run' in args\n"
            "    cwd = pathlib.Path(args[args.index('--cwd') + 1])\n"
            "    assert cwd == cwd.resolve() and cwd == pathlib.Path.cwd()\n"
            "    executable = pathlib.Path(args[args.index('--') + 1])\n"
            "    assert executable == executable.resolve()\n"
            "    root = pathlib.Path(args[args.index('--root') + 1])\n"
            f"    if {creates_root!r}: root.mkdir()\n"
            f"    print(json.dumps({{'schema_version': 'loboot.cli/v1', 'ok': True, 'data': {{'dry_run': True, 'native_commands': {native_commands or []!r}}}}}))\n",
            encoding="utf-8",
        )
        self.binary.chmod(0o755)

    def build(self, target=LINUX, *, name=None):
        output = self.root / (name or target)
        with patch.object(release, "require_native_target") as native:
            metadata = release.build(self.repo, target, output)
        native.assert_called_once_with(target)
        return output, metadata

    def license(self):
        self.cargo('license = "MIT"\n')
        (self.repo / "LICENSE-MIT").write_text("MIT License\nFixture license text.\n", encoding="utf-8")

    def rewrite_archive(self, path, change):
        with tarfile.open(path, "r:gz") as archive:
            entries = []
            for member in archive.getmembers():
                source = archive.extractfile(member) if member.isfile() else None
                entries.append((member, source.read() if source else None))
        entries = change(entries)
        with tarfile.open(path, "w:gz") as archive:
            for member, content in entries:
                archive.addfile(member, io.BytesIO(content) if content is not None else None)

    def test_strict_versions(self):
        for version in ("0.1.0", "1.2.3", "10.20.300"):
            self.assertEqual(release.validate_version(version), version)
        for version in (None, 1, "", "v0.1.0", "01.0.0", "0.01.0", "0.1.00", "0.1", "1.2.3-rc.1", "1.2.3+build", "../0.1.0", "0.1.0\n", "/tmp/0.1.0"):
            with self.subTest(version=version), self.assertRaises(release.ReleaseError):
                release.validate_version(version)

    def test_tag_must_match(self):
        release.validate_tag(None, "0.1.0")
        release.validate_tag("v0.1.0", "0.1.0")
        for tag in ("0.1.0", "v0.2.0", "v0.1.0\n", "refs/tags/v0.1.0", "../../escape", ""):
            with self.subTest(tag=tag), self.assertRaises(release.ReleaseError):
                release.validate_tag(tag, "0.1.0")

    def test_unsupported_target(self):
        for target in ("x86_64-pc-windows-msvc", "aarch64-unknown-linux-musl", "../../escape"):
            with self.subTest(target=target), self.assertRaises(release.ReleaseError):
                release.archive_stem("0.1.0", target)

    def test_native_host_is_exact(self):
        with patch.object(release, "run", return_value=f"rustc 1.99.0\nhost: {LINUX}\n"):
            release.require_native_target(LINUX)
            with self.assertRaisesRegex(release.ReleaseError, "no cross-linking"):
                release.require_native_target(MACOS)
        for text in ("host missing", f"host: {LINUX}\nhost: {LINUX}\n"):
            with patch.object(release, "run", return_value=text), self.assertRaises(release.ReleaseError):
                release.require_native_target(LINUX)

    def test_binary_header_accepts_only_native_executable_architecture(self):
        linux = bytearray(64)
        linux[:7] = b"\x7fELF\x02\x01\x01"
        linux[16:18] = (3).to_bytes(2, "little")
        linux[18:20] = (62).to_bytes(2, "little")
        macos = bytearray(32)
        macos[:4] = b"\xcf\xfa\xed\xfe"
        macos[4:8] = (0x0100000C).to_bytes(4, "little")
        macos[12:16] = (2).to_bytes(4, "little")
        self.header_validator(bytes(linux), LINUX)
        self.header_validator(bytes(macos), MACOS)
        for header, target in ((linux, MACOS), (macos, LINUX), (b"#!/bin/sh\n", LINUX), (b"", MACOS)):
            with self.subTest(target=target), self.assertRaisesRegex(release.ReleaseError, "binary header"):
                self.header_validator(bytes(header), target)
        for header, offset, value, target in (
            (linux, 4, 1, LINUX),  # ELF32
            (linux, 18, 183, LINUX),  # ARM64 Linux
            (linux, 16, 1, LINUX),  # relocatable object
            (macos, 4, 7, MACOS),  # x86_64 under Rosetta
            (macos, 12, 6, MACOS),  # dynamic library
        ):
            invalid = bytearray(header)
            invalid[offset] = value
            with self.subTest(target=target, offset=offset), self.assertRaises(release.ReleaseError):
                self.header_validator(bytes(invalid), target)

    def test_all_four_native_headers(self):
        for target in release.TARGETS:
            if "linux" in target:
                header = bytearray(64)
                header[:7] = b"\x7fELF\x02\x01\x01"
                header[16:18] = (3).to_bytes(2, "little")
                header[18:20] = (62 if target.startswith("x86_64") else 183).to_bytes(2, "little")
            else:
                header = bytearray(32)
                header[:4] = b"\xcf\xfa\xed\xfe"
                header[4:8] = (0x01000007 if target.startswith("x86_64") else 0x0100000C).to_bytes(4, "little")
                header[12:16] = (2).to_bytes(4, "little")
            self.header_validator(bytes(header), target)
            for other in release.TARGETS:
                if other != target:
                    with self.subTest(target=target, other=other), self.assertRaises(release.ReleaseError):
                        self.header_validator(bytes(header), other)

    def test_build_and_archive_verification_check_architecture(self):
        self.build()
        self.assertGreaterEqual(self.header_mock.call_count, 3)
        self.assertTrue(all(call.args[1] == LINUX for call in self.header_mock.call_args_list))
        self.header_mock.side_effect = release.ReleaseError("binary header mismatch")
        with patch.object(release, "require_native_target"), self.assertRaisesRegex(release.ReleaseError, "binary header"):
            release.build(self.repo, MACOS, self.root / "wrong-architecture")

    def test_package_readme_is_self_contained_with_absolute_documentation_links(self):
        for tag, reference in ((None, "main"), ("v0.1.0", "v0.1.0")):
            readme = release.package_readme("0.1.0", LINUX, tag).decode()
            self.assertIn("Manual installation", readme)
            self.assertIn("LICENSE-STATUS.txt", readme)
            self.assertNotIn("](docs/", readme)
            self.assertIn(f"{release.SOURCE_REPOSITORY}/blob/{reference}/docs/INSTALL.md", readme)

    def test_package_reads_toml_instead_of_grep(self):
        self.cargo('# version = "9.9.9"\n[dependencies]\nexample = { version = "7.0.0" }\n')
        self.assertEqual(release.read_package(self.repo)["version"], "0.1.0")
        self.cargo(version="../escape")
        with self.assertRaises(release.ReleaseError):
            release.read_package(self.repo)

    def test_build_round_trip_and_metadata(self):
        output, metadata = self.build()
        self.assertEqual(metadata["version"], "0.1.0")
        self.assertEqual(metadata["target"], LINUX)
        archive = output / metadata["archive"]
        self.assertEqual(metadata["sha256"], release.digest(archive))
        self.assertEqual((output / "SHA256SUMS.txt").read_text(), f"{metadata['sha256']}  {archive.name}\n")
        self.assertEqual(json.loads((output / "manifest.json").read_text()), metadata)
        with patch.object(release, "require_native_target"):
            self.assertEqual(release.check(self.repo, LINUX, output, "v0.1.0"), metadata)
        with tarfile.open(archive) as tar:
            stem = release.archive_stem("0.1.0", LINUX)
            self.assertEqual(tar.getmember(f"{stem}/loboot").mode, 0o755)
            self.assertEqual(tar.extractfile(f"{stem}/LICENSE-STATUS.txt").read().decode(), release.PENDING_LICENSE)

    def test_deterministic_archive_independent_of_file_mtimes(self):
        first, one = self.build(name="one")
        os.utime(self.binary, (5000, 5000))
        os.utime(self.repo / "README.md", (9000, 9000))
        second, two = self.build(name="two")
        self.assertEqual((first / one["archive"]).read_bytes(), (second / two["archive"]).read_bytes())
        self.assertEqual(one, two)
        self.assertEqual((first / one["archive"]).read_bytes()[4:8], b"\x00" * 4)

    def test_build_rejects_wrong_binary_version(self):
        self.fake_binary(version="0.2.0")
        with patch.object(release, "require_native_target"), self.assertRaisesRegex(release.ReleaseError, "binary version mismatch"):
            release.build(self.repo, LINUX, self.root / "output")
        self.assertFalse((self.root / "output").exists())

    def test_build_rejects_symlink_binary(self):
        actual = self.binary.with_name("actual")
        self.binary.rename(actual)
        self.binary.symlink_to(actual)
        with patch.object(release, "require_native_target"), self.assertRaisesRegex(release.ReleaseError, "not a link"):
            release.build(self.repo, LINUX, self.root / "output")

    def test_build_rejects_nonexecutable_binary(self):
        self.binary.chmod(0o644)
        with patch.object(release, "require_native_target"), self.assertRaisesRegex(release.ReleaseError, "executable"):
            release.build(self.repo, LINUX, self.root / "output")

    def test_build_does_not_overwrite_outputs(self):
        output, _ = self.build()
        with patch.object(release, "require_native_target"), self.assertRaisesRegex(release.ReleaseError, "empty directory"):
            release.build(self.repo, LINUX, output)

    def test_smoke_uses_only_offline_dryrun_for_both_platforms(self):
        for target in release.TARGETS:
            release.smoke(self.binary, "0.1.0", target)

    def test_smoke_detects_root_creation(self):
        self.fake_binary(creates_root=True)
        with self.assertRaisesRegex(release.ReleaseError, "created its root"):
            release.smoke(self.binary, "0.1.0", LINUX)

    def test_smoke_rejects_native_commands(self):
        self.fake_binary(native_commands=["systemctl"])
        with self.assertRaisesRegex(release.ReleaseError, "native-command-free"):
            release.smoke(self.binary, "0.1.0", LINUX)

    def test_archive_rejects_traversal_absolute_and_duplicate_names(self):
        output, metadata = self.build()
        original = (output / metadata["archive"]).read_bytes()
        archive = output / metadata["archive"]
        for name in ("../../escape", "/tmp/escape", release.archive_stem("0.1.0", LINUX)):
            archive.write_bytes(original)
            def change(entries):
                entries[1][0].name = name
                return entries
            self.rewrite_archive(archive, change)
            with self.subTest(name=name), self.assertRaisesRegex(release.ReleaseError, "inventory"):
                release.verify_archive(archive, "0.1.0", LINUX, execute=True)
        self.assertFalse((self.root / "escape").exists())

    def test_archive_rejects_links_and_devices(self):
        output, metadata = self.build()
        archive = output / metadata["archive"]
        original = archive.read_bytes()
        for member_type in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.FIFOTYPE):
            archive.write_bytes(original)
            def change(entries):
                member = entries[1][0]
                member.type = member_type
                member.linkname = "/tmp/escape"
                member.size = 0
                entries[1] = (member, None)
                return entries
            self.rewrite_archive(archive, change)
            with self.subTest(type=member_type), self.assertRaisesRegex(release.ReleaseError, "special files"):
                release.verify_archive(archive, "0.1.0", LINUX, execute=True)

    def test_archive_rejects_wrong_permissions_and_metadata(self):
        output, metadata = self.build()
        archive = output / metadata["archive"]
        original = archive.read_bytes()
        for field, value in (("mode", 0o644), ("mode", 0o4755), ("uid", 1000), ("mtime", 1), ("uname", "runner")):
            archive.write_bytes(original)
            def change(entries):
                setattr(entries[1][0], field, value)
                return entries
            self.rewrite_archive(archive, change)
            with self.subTest(field=field, value=value), self.assertRaises(release.ReleaseError):
                release.verify_archive(archive, "0.1.0", LINUX, execute=False)

    def test_bundle_rejects_tampered_manifest_checksum_and_archive(self):
        output, metadata = self.build()
        manifest = output / "manifest.json"
        original_manifest = manifest.read_bytes()
        for field, value in (("version", "9.9.9"), ("target", MACOS), ("archive", "../escape"), ("sha256", "0" * 64)):
            modified = dict(metadata, **{field: value})
            release.dump_json(manifest, modified)
            with self.subTest(field=field), self.assertRaisesRegex(release.ReleaseError, "manifest"):
                release.verify_bundle(output, "0.1.0", LINUX, execute=False)
        manifest.write_bytes(original_manifest)
        checksum = output / "SHA256SUMS.txt"
        original_checksum = checksum.read_bytes()
        checksum.write_text(f"{metadata['sha256']}  {output / metadata['archive']}\n")
        with self.assertRaisesRegex(release.ReleaseError, "bare archive"):
            release.verify_bundle(output, "0.1.0", LINUX, execute=False)
        checksum.write_bytes(original_checksum)
        with (output / metadata["archive"]).open("ab") as archive:
            archive.write(b"tampered")
        with self.assertRaisesRegex(release.ReleaseError, "manifest"):
            release.verify_bundle(output, "0.1.0", LINUX, execute=False)

    def test_notes_extract_only_the_exact_curated_version(self):
        (self.repo / "CHANGELOG.md").write_text(
            "# Changelog\n## [Unreleased]\nFuture.\n## [0.1.0] - 2026-10-13\n\n### Added\n- Exact release.\n\n## [0.0.9]\nOld.\n")
        self.assertEqual(release.release_notes(self.repo, "v0.1.0"), "### Added\n- Exact release.\n")
        with self.assertRaisesRegex(release.ReleaseError, "exactly"):
            release.release_notes(self.repo, "v0.2.0")

    def test_notes_reject_ambiguous_or_empty_sections(self):
        for text in ("## [0.1.0]\nOne.\n## [0.1.0]\nTwo.\n", "## [0.1.0]\n\n## [0.0.9]\nOld.\n"):
            (self.repo / "CHANGELOG.md").write_text(text)
            with self.subTest(text=text), self.assertRaises(release.ReleaseError):
                release.release_notes(self.repo, "v0.1.0")

    def test_notes_cli_never_overwrites_an_existing_file(self):
        output = self.root / "notes.md"
        argv = ["notes", "--repo", str(self.repo), "--tag", "v0.1.0", "--output", str(output)]
        with redirect_stdout(io.StringIO()):
            self.assertEqual(release.main(argv), 0)
        self.assertEqual(output.read_text(), "Initial version.\n")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(release.main(argv), 1)
        self.assertEqual(output.read_text(), "Initial version.\n")

    def test_publication_trigger_matches_locron_without_main_release(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github/workflows/release.yml").read_text()
        triggers = workflow.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertIn("  push:\n    tags:\n      - 'v*.*.*'", triggers)
        self.assertIn("workflow_dispatch:", triggers)
        self.assertIn("expected_commit:", triggers)
        self.assertIn('test "$GITHUB_REF" = refs/heads/main', workflow)
        self.assertIn('test "$actual" = "$EXPECTED_COMMIT"', workflow)
        self.assertIn('test "$current_tag_sha" = "$RELEASE_COMMIT"', workflow)
        self.assertIn("Create the approved annotated tag only after readiness passes", workflow)
        self.assertNotIn("inputs.publish", workflow)
        self.assertIn('--notes-file release-notes.md', workflow)
        self.assertNotIn('--generate-notes', workflow)
        self.assertIn('test "$GITHUB_EVENT_NAME" = push', workflow)
        self.assertIn('test "$GITHUB_REF_TYPE" = tag', workflow)
        self.assertIn('needs: [gate, readiness]', workflow)
        self.assertEqual(workflow.count('contents: write'), 1)
        readiness = (Path(__file__).resolve().parents[1] / ".github/workflows/release-readiness.yml").read_text()
        for mutator in ('gh release create', 'cargo publish', 'git push'):
            self.assertNotIn(mutator, readiness)

    def test_gate_fails_for_pending_license(self):
        with self.assertRaisesRegex(release.ReleaseError, "select license"):
            release.gate(self.repo, "v0.1.0")

    def test_gate_requires_license_text_and_matching_changelog(self):
        self.cargo('license = "MIT"\n')
        with self.assertRaisesRegex(release.ReleaseError, "add a chosen"):
            release.gate(self.repo, "v0.1.0")
        (self.repo / "LICENSE-MIT").write_text("")
        with self.assertRaisesRegex(release.ReleaseError, "empty"):
            release.gate(self.repo, "v0.1.0")
        self.license()
        for heading in ("## [Unreleased]", "### [0.1.0]", "## [0.1.01]", "## 0.1.0-rc.1", "version 0.1.0"):
            (self.repo / "CHANGELOG.md").write_text(heading + "\n")
            with self.subTest(heading=heading), self.assertRaisesRegex(release.ReleaseError, "section"):
                release.gate(self.repo, "v0.1.0")

    def test_gate_accepts_publish_false_and_includes_chosen_legal_text(self):
        self.license()
        result = release.gate(self.repo, "v0.1.0")
        self.assertTrue(result["ready"])
        self.assertEqual(result["license_files"], ["LICENSE-MIT"])
        output, metadata = self.build()
        with tarfile.open(output / metadata["archive"]) as archive:
            status = archive.extractfile(f"{release.archive_stem('0.1.0', LINUX)}/LICENSE-STATUS.txt").read().decode()
        self.assertIn("License selected in Cargo.toml: MIT", status)
        self.assertIn("Fixture license text.", status)
        self.assertNotIn("pending", status)

    def test_dual_license_requires_both_texts(self):
        self.cargo('license = "MIT OR Apache-2.0"\n')
        (self.repo / "LICENSE-MIT").write_text("MIT text")
        with self.assertRaisesRegex(release.ReleaseError, "both"):
            release.gate(self.repo, "v0.1.0")
        (self.repo / "LICENSE-APACHE").write_text("Apache text")
        self.assertEqual(len(release.gate(self.repo, "v0.1.0")["license_files"]), 2)

    def test_gate_supports_safe_license_file(self):
        self.cargo('license-file = "LICENSE"\n')
        (self.repo / "LICENSE").write_text("Chosen license text.\n")
        self.assertEqual(release.gate(self.repo, "v0.1.0")["license_files"], ["LICENSE"])
        for name in ("../LICENSE", "/tmp/LICENSE", "LICENSE-STATUS.txt"):
            self.cargo(f'license-file = "{name}"\n')
            with self.subTest(name=name), self.assertRaisesRegex(release.ReleaseError, "must name"):
                release.gate(self.repo, "v0.1.0")

    def test_gate_rejects_wrong_tag(self):
        self.license()
        with self.assertRaisesRegex(release.ReleaseError, "exactly"):
            release.gate(self.repo, "v0.2.0")

    def downloaded(self):
        artifacts = self.root / "downloaded"
        artifacts.mkdir()
        for target in release.TARGETS:
            output, _ = self.build(target)
            shutil.move(str(output), artifacts / f"job-{target}")
        return artifacts

    def test_collect_both_targets_without_executing_foreign_binaries(self):
        artifacts = self.downloaded()
        with patch.object(release, "smoke", side_effect=AssertionError("collector must not run binaries")):
            metadata = release.collect(self.repo, artifacts, self.root / "publish", "0.1.0")
        self.assertEqual([item["target"] for item in metadata["artifacts"]], sorted(release.TARGETS))
        output = self.root / "publish"
        self.assertEqual(len(list(output.iterdir())), len(release.TARGETS) + 2)
        self.assertEqual(json.loads((output / "manifest.json").read_text()), metadata)
        for line in (output / "SHA256SUMS.txt").read_text().splitlines():
            expected, filename = line.split("  ")
            self.assertEqual(Path(filename).name, filename)
            self.assertEqual(release.digest(output / filename), expected)

    def test_collect_rejects_missing_target(self):
        artifacts = self.downloaded()
        shutil.rmtree(artifacts / f"job-{MACOS}")
        with self.assertRaisesRegex(release.ReleaseError, "exactly one"):
            release.collect(self.repo, artifacts, self.root / "publish", "0.1.0")

    def test_collect_rejects_duplicate_target(self):
        artifacts = self.downloaded()
        shutil.copytree(artifacts / f"job-{LINUX}", artifacts / "duplicate")
        with self.assertRaisesRegex(release.ReleaseError, "exactly one"):
            release.collect(self.repo, artifacts, self.root / "publish", "0.1.0")

    def test_collect_rejects_unexpected_files_archives_and_links(self):
        artifacts = self.downloaded()
        for filename in ("unexpected.tar.gz", "extra.txt"):
            extra = artifacts / filename
            extra.write_text("unexpected")
            with self.subTest(filename=filename), self.assertRaisesRegex(release.ReleaseError, "unexpected"):
                release.collect(self.repo, artifacts, self.root / "publish", "0.1.0")
            extra.unlink()
        (artifacts / "link").symlink_to(self.repo)
        with self.assertRaisesRegex(release.ReleaseError, "link"):
            release.collect(self.repo, artifacts, self.root / "publish", "0.1.0")
        self.assertFalse((self.root / "publish").exists())

    def test_collect_rejects_version_mismatch(self):
        with self.assertRaisesRegex(release.ReleaseError, "Cargo.toml"):
            release.collect(self.repo, self.root, self.root / "publish", "0.2.0")

    def test_main_emits_json_or_concise_error(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with redirect_stdout(stdout), redirect_stderr(stderr):
            self.assertEqual(release.main(["gate", "--repo", str(self.repo), "--tag", "v0.1.0"]), 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertTrue(stderr.getvalue().startswith("release: "))
        self.license()
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            self.assertEqual(release.main(["gate", "--repo", str(self.repo), "--tag", "v0.1.0"]), 0)
        self.assertEqual(json.loads(stdout.getvalue())["version"], "0.1.0")

    def test_run_uses_timeout_and_reports_failure(self):
        failure = subprocess.CompletedProcess(["fake"], 1, "", "fixture error")
        with patch.object(subprocess, "run", return_value=failure) as runner:
            with self.assertRaisesRegex(release.ReleaseError, "fixture error"):
                release.run(["fake"])
        self.assertEqual(runner.call_args.kwargs["timeout"], 30)
        self.assertFalse(runner.call_args.kwargs["check"])


if __name__ == "__main__":
    unittest.main()
