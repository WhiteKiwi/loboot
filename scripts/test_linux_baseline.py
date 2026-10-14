#!/usr/bin/env python3
"""Offline parser, evidence, architecture, and workflow tests; no live services."""
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import linux_baseline as baseline
import release

X64 = "x86_64-unknown-linux-gnu"
ARM = "aarch64-unknown-linux-gnu"


def outputs(versions=("GLIBC_2.2.5", "GLIBC_2.35"), flags="none", binding="GLOBAL"):
    info = f"""Version symbols section '.gnu.version' contains {len(versions) + 1} entries:
 Addr: 0x100 Offset: 0x100 Link: 5 (.dynsym)
 000: 0 (*local*) """ + " ".join(f"{index + 2:x} ({name})" for index, name in enumerate(versions)) + f"""

Version needs section '.gnu.version_r' contains 1 entry:
 Addr: 0x200 Offset: 0x200 Link: 6 (.dynstr)
 000000: Version: 1 File: libc.so.6 Cnt: {len(versions)}
"""
    info += "".join(f" 0x{16 * (index + 1):04x}: Name: {name} Flags: {flags} Version: {index + 2}\n" for index, name in enumerate(versions))
    symbols = f"""Symbol table '.dynsym' contains {len(versions) + 1} entries:
 Num: Value Size Type Bind Vis Ndx Name
 0: 0000000000000000 0 NOTYPE LOCAL DEFAULT UND
"""
    symbols += "".join(f" {index + 1}: 0000000000000000 0 FUNC {binding} DEFAULT UND symbol{index}@{name} ({index + 2})\n" for index, name in enumerate(versions))
    dynamic = """Dynamic section at offset 0x300 contains 2 entries:
 Tag Type Name/Value
 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]
 0x0000000000000000 (NULL) 0x0
"""
    return {"--version": "GNU readelf (GNU Binutils) 2.38\n", "--version-info": info, "--dyn-syms": symbols, "--dynamic": dynamic}


def elf(target=X64):
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    header[16:18] = (3).to_bytes(2, "little")
    header[18:20] = (62 if target == X64 else 183).to_bytes(2, "little")
    return bytes(header) + b"fixture-not-an-executable"


class ParserTests(unittest.TestCase):
    def test_numeric_version_ordering_and_patch(self):
        self.assertLess(baseline.numeric_version("2.9"), baseline.numeric_version("2.35"))
        self.assertLess(baseline.numeric_version("2.35"), baseline.numeric_version("2.35.1"))
        self.assertEqual(baseline.numeric_version("2.35"), baseline.numeric_version("2.35.0"))
        for value in ("2", "2.", "2.35.", "2.35.1.2", "02.35", "2.035", "2.-1", "2.35junk", " 2.35", "2.35\n"):
            with self.subTest(value=value), self.assertRaises(baseline.BaselineError):
                baseline.numeric_version(value)

    def test_versions_at_or_below_ceiling_including_patches(self):
        for version in ("2.2.5", "2.9", "2.34.9", "2.35", "2.35.0"):
            needs = baseline.parse_version_needs(outputs((f"GLIBC_{version}",))["--version-info"])
            self.assertEqual(baseline.check_ceiling(needs), version)

    def test_newer_weak_references_are_rejected_without_calling_them_strong(self):
        for version in ("2.35.1", "2.39", "2.100", "3.0"):
            raw = outputs((f"GLIBC_{version}",), flags="WEAK", binding="WEAK")
            needs = baseline.parse_version_needs(raw["--version-info"])
            symbols = baseline.parse_symbols(raw["--dyn-syms"], needs)
            self.assertEqual(needs[0]["flags"], "WEAK")
            self.assertEqual(symbols[0]["binding"], "WEAK")
            with self.subTest(version=version), self.assertRaisesRegex(baseline.BaselineError, "including weak"):
                baseline.check_ceiling(needs)

    def test_unknown_names_are_never_ignored_or_partially_matched(self):
        for name in ("GLIBC_PRIVATE", "GLIBC_ABI_DT_RELR", "GLIBC_2.35x", "GLIBC_2.35.1.0", "GLIBC_02.35", "GLIBC_", "GLIBCXX_3.4", "MYSTERY_1.0"):
            with self.subTest(name=name), self.assertRaises(baseline.BaselineError):
                baseline.parse_version_needs(outputs(("GLIBC_2.2.5", name))["--version-info"])

    def test_missing_malformed_and_truncated_version_info(self):
        info = outputs()["--version-info"]
        bad = ("", "No version information found in this file.", info.replace("Name:", "Names:", 1),
               info.replace("Cnt: 2", "Cnt: 3"), info.replace("contains 1 entry:", "contains 2 entries:"),
               info.replace("Flags: none", "Flags: unknown", 1), info.rsplit("\n", 2)[0],
               info.replace("Version: 3", "Version: 2"), info.replace(".dynstr", ".missing"),
               info + "malformed trailing output\n", info + "GLIBC_PRIVATE\n",
               info.replace("(GLIBC_2.35)", "(GLIBC_2.36)"),
               info.replace("contains 3 entries:", "contains 999 entries:"),
               "Version symbols section '.gnu.version' contains 999 entries:\n\n" + info.split("Version needs section", 1)[1],
               "\n".join(line for line in info.splitlines() if not line.startswith(" 000:")),
               info.replace(" 000:", " 004:", 1))
        for text in bad:
            with self.subTest(text=text), self.assertRaises(baseline.BaselineError):
                baseline.parse_version_needs(text)

    def test_dynsym_missing_malformed_and_disagrees_with_needs(self):
        raw = outputs()
        needs = baseline.parse_version_needs(raw["--version-info"])
        symbols = raw["--dyn-syms"]
        for text in ("", symbols.replace("contains 3", "contains 4"), symbols.replace("Name", "Wrong"),
                     symbols.replace("symbol1@GLIBC_2.35", "symbol1@GLIBC_PRIVATE"),
                     symbols.replace("(3)", "(9)"), symbols.replace("GLOBAL", "UNKNOWN", 1),
                     symbols.replace("symbol1@", "symbol1@@"), symbols + "garbage\n"):
            with self.subTest(text=text), self.assertRaises(baseline.BaselineError):
                baseline.parse_symbols(text, needs)

    def test_defined_copy_relocation_symbols_are_still_inspected(self):
        raw = outputs()
        needs = baseline.parse_version_needs(raw["--version-info"])
        symbols = baseline.parse_symbols(raw["--dyn-syms"].replace("GLOBAL DEFAULT UND", "GLOBAL DEFAULT 24"), needs)
        self.assertTrue(all(symbol["section"] == "24" for symbol in symbols))

    def test_symbol_count_and_index_mismatch_refused(self):
        raw = outputs()
        needs = baseline.parse_version_needs(raw["--version-info"])
        indexes = baseline.parse_version_symbols(raw["--version-info"], needs)
        self.assertEqual(indexes, [0, 2, 3])
        for bad in ([0, 2], [0, 3, 2], [2, 2, 3]):
            with self.subTest(indexes=bad), self.assertRaises(baseline.BaselineError):
                baseline.parse_symbols(raw["--dyn-syms"], needs, bad)

    def test_multiple_libraries_with_gcc_namespace(self):
        raw = outputs(("GLIBC_2.35", "GCC_3.0"))
        info = raw["--version-info"].replace("contains 1 entry:", "contains 2 entries:").replace("libc.so.6 Cnt: 2", "libc.so.6 Cnt: 1")
        info = info.replace(" 0x0020: Name: GCC_3.0", " 0x0030: Version: 1 File: libgcc_s.so.1 Cnt: 1\n 0x0020: Name: GCC_3.0")
        needs = baseline.parse_version_needs(info)
        self.assertEqual(needs[1]["library"], "libgcc_s.so.1")
        self.assertEqual(baseline.check_ceiling(needs), "2.35")
        baseline.parse_symbols(raw["--dyn-syms"], needs, baseline.parse_version_symbols(info, needs))
        dynamic = raw["--dynamic"].replace("contains 2 entries:", "contains 3 entries:") + " 0x0000000000000001 (NEEDED) Shared library: [libgcc_s.so.1]\n"
        self.assertEqual(baseline.parse_dynamic(dynamic, needs), ["libc.so.6", "libgcc_s.so.1"])

    def test_dynamic_requirements_fail_closed(self):
        raw = outputs()
        needs = baseline.parse_version_needs(raw["--version-info"])
        dynamic = raw["--dynamic"]
        self.assertEqual(baseline.parse_dynamic(dynamic, needs), ["libc.so.6"])
        for text in ("", "There is no dynamic section in this file.", dynamic.replace("libc.so.6", "other.so"),
                     dynamic.replace("[libc.so.6]", "[../libc.so.6]"), dynamic.replace("contains 2", "contains 3"),
                     dynamic.replace("(NULL)", "(RELR)"), dynamic.replace("(NULL)", "(RPATH)"),
                     dynamic + "trailing garbage\n"):
            with self.subTest(text=text), self.assertRaises(baseline.BaselineError):
                baseline.parse_dynamic(text, needs)


class ExecutionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / "loboot"
        self.binary.write_bytes(elf())

    def mock_readelf(self, raw=None):
        raw = raw or outputs()
        return patch.object(baseline, "command", side_effect=lambda argv, evidence=None: raw[argv[1]])

    def test_both_native_architectures_and_tool_argv(self):
        for target in (X64, ARM):
            self.binary.write_bytes(elf(target))
            evidence = {}
            with self.mock_readelf() as command:
                baseline.inspect_binary(self.binary, target, evidence)
            self.assertEqual(evidence["max_glibc_reference"], "2.35")
            self.assertEqual([call.args[0] for call in command.call_args_list], [
                ["readelf", "--version"], ["readelf", "--version-info", "--wide", str(self.binary)],
                ["readelf", "--dyn-syms", "--wide", str(self.binary)], ["readelf", "--dynamic", "--wide", str(self.binary)]])

    def test_wrong_architecture_refused_before_tooling_or_execution(self):
        with self.mock_readelf() as command, self.assertRaises(release.ReleaseError):
            baseline.inspect_binary(self.binary, ARM, {})
        command.assert_not_called()

    def test_non_gnu_or_missing_readelf_fails(self):
        raw = outputs()
        raw["--version"] = "LLVM readelf version 18\n"
        with self.mock_readelf(raw), self.assertRaisesRegex(baseline.BaselineError, "GNU readelf"):
            baseline.inspect_binary(self.binary, X64, {})
        with patch.object(baseline, "command", side_effect=FileNotFoundError("readelf")), self.assertRaises(FileNotFoundError):
            baseline.inspect_binary(self.binary, X64, {})

    def test_command_locale_timeout_raw_failures_and_missing_tooling(self):
        for result in (subprocess.CompletedProcess([], 1, "partial", "error"),
                       subprocess.CompletedProcess([], 0, "", ""),
                       subprocess.CompletedProcess([], 0, "partial", "warning")):
            evidence = {}
            with patch.object(subprocess, "run", return_value=result) as run, self.assertRaises(baseline.BaselineError):
                baseline.command(["readelf", "--version"], evidence)
            self.assertEqual(run.call_args.kwargs["env"]["LC_ALL"], "C")
            self.assertEqual(run.call_args.kwargs["timeout"], 30)
            self.assertEqual(evidence["commands"][0]["stderr"], result.stderr)
            self.assertEqual(evidence["commands"][0]["stdout"], result.stdout)
        for error in (FileNotFoundError("missing readelf"), subprocess.TimeoutExpired("readelf", 30)):
            evidence = {}
            with patch.object(subprocess, "run", side_effect=error), self.assertRaises(type(error)):
                baseline.command(["readelf"], evidence)
            self.assertIn("error", evidence["commands"][0])

    def make_bundle(self, target=X64):
        directory = self.root / "dist"
        directory.mkdir()
        stem = release.archive_stem("0.1.0", target)
        archive = directory / f"{stem}.tar.gz"
        contents = {name: b"fixture\n" for name in release.ARCHIVE_FILES}
        contents["loboot"] = elf(target)
        release.write_archive(archive, stem, contents)
        metadata = release.bundle_metadata("0.1.0", target, archive)
        release.dump_json(directory / "manifest.json", metadata)
        (directory / "SHA256SUMS.txt").write_text(f"{metadata['sha256']}  {archive.name}\n")
        return directory

    def test_checks_and_smokes_the_extracted_archive_not_checkout_binary(self):
        directory = self.make_bundle()
        evidence = {}
        def smoke(binary, version, target):
            self.assertEqual(binary.read_bytes(), elf())
            self.assertNotEqual(binary, self.binary)
            self.assertEqual((version, target), ("0.1.0", X64))
        with self.mock_readelf(), patch.object(release, "smoke", side_effect=smoke) as mocked:
            baseline.check_bundle(directory, X64, evidence)
        mocked.assert_called_once()
        self.assertEqual(evidence["extracted_archive_offline_smoke"], "passed")
        self.assertEqual(set(path.name for path in directory.iterdir()), {evidence["bundle"]["archive"], "manifest.json", "SHA256SUMS.txt"})

    def test_failed_archive_smoke_is_not_reported_as_unrun(self):
        directory = self.make_bundle()
        evidence = {}
        with self.mock_readelf(), patch.object(release, "smoke", side_effect=release.ReleaseError("smoke failed")), self.assertRaises(release.ReleaseError):
            baseline.check_bundle(directory, X64, evidence)
        self.assertEqual(evidence["extracted_archive_offline_smoke"], "failed")

    def test_newer_abi_cannot_reach_offline_smoke(self):
        directory = self.make_bundle()
        evidence = {}
        with self.mock_readelf(outputs(("GLIBC_2.39",), flags="WEAK", binding="WEAK")), patch.object(release, "smoke") as smoke, self.assertRaises(baseline.BaselineError):
            baseline.check_bundle(directory, X64, evidence)
        smoke.assert_not_called()
        self.assertEqual(evidence["versioned_symbols"][0]["binding"], "WEAK")
        self.assertEqual(evidence["version_needs"][0]["flags"], "WEAK")
        self.assertEqual(evidence["max_glibc_reference"], "2.39")

    def test_non_object_manifest_fails_and_records_evidence(self):
        directory = self.make_bundle()
        report = self.root / "evidence.json"
        args = ["--input", str(directory), "--target", X64, "--runner-label", "ubuntu-22.04", "--report", str(report)]
        for value in ([], None, "wrong"):
            release.dump_json(directory / "manifest.json", value)
            with patch.object(baseline, "environment_evidence"), redirect_stderr(io.StringIO()):
                self.assertEqual(baseline.main(args), 1)
            self.assertIn("JSON object", json.loads(report.read_text())["error"])
            report.unlink()

    def test_strict_archive_inventory_still_rejects_extra_evidence(self):
        directory = self.make_bundle()
        (directory / "baseline.json").write_text("{}")
        with self.mock_readelf() as command, self.assertRaises(release.ReleaseError):
            baseline.check_bundle(directory, X64, {})
        command.assert_not_called()

    def test_report_success_failure_refusal_and_no_overwrite(self):
        directory = self.make_bundle()
        report = self.root / "evidence.json"
        args = ["--input", str(directory), "--target", X64, "--runner-label", "ubuntu-22.04", "--report", str(report)]
        with patch.object(baseline, "environment_evidence"), self.mock_readelf(), patch.object(release, "smoke"), redirect_stdout(io.StringIO()):
            self.assertEqual(baseline.main(args), 0)
        self.assertEqual(json.loads(report.read_text())["status"], "passed")
        original = report.read_bytes()
        with redirect_stderr(io.StringIO()):
            self.assertEqual(baseline.main(args), 1)
        self.assertEqual(report.read_bytes(), original)
        report.unlink()
        with patch.object(baseline, "environment_evidence", side_effect=FileNotFoundError("rustc unavailable")), redirect_stderr(io.StringIO()):
            self.assertEqual(baseline.main(args), 1)
        result = json.loads(report.read_text())
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["extracted_archive_offline_smoke"], "not-run")
        self.assertIn("rustc", result["error"])
        args[-1] = str(directory / "evidence.json")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(baseline.main(args), 1)
        self.assertFalse((directory / "evidence.json").exists())


class EnvironmentTests(unittest.TestCase):
    def check(self, target=X64, *, distro=None, glibc="glibc 2.35", rust="1.99.0", cargo="1.99.0", machine=None, label=None, image="20261001.1"):
        expected_label, expected_machine, arch = baseline.TARGETS[target]
        def output(argv, evidence=None):
            return {"getconf": glibc, "rustc": f"rustc {rust} (fixture)\nhost: {target}\n",
                    "cargo": f"cargo {cargo} (fixture)", "cc": "cc version", "ld": "GNU ld version",
                    "uname": "Linux fixture", "git": "a" * 40}[argv[0]]
        evidence = {}
        with patch.object(baseline.platform, "system", return_value="Linux"), patch.object(baseline.platform, "machine", return_value=machine or expected_machine), patch.object(baseline.platform, "freedesktop_os_release", return_value=distro or {"ID": "ubuntu", "VERSION_ID": "22.04"}), patch.object(baseline, "command", side_effect=output), patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", "RUNNER_ARCH": arch, "ImageVersion": image}, clear=True):
            baseline.environment_evidence(target, label or expected_label, evidence)
        return evidence

    def test_exact_baseline_both_architectures(self):
        for target in (X64, ARM):
            evidence = self.check(target)
            self.assertEqual(evidence["environment"]["glibc"], "glibc 2.35")
            self.assertEqual(evidence["environment"]["runner"]["ImageVersion"], "20261001.1")
            self.assertEqual(evidence["environment"]["source_commit"], "a" * 40)

    def test_wrong_os_glibc_toolchain_arch_or_missing_image_refused(self):
        for changes in ({"distro": {"ID": "ubuntu", "VERSION_ID": "24.04"}}, {"glibc": "glibc 2.39"},
                        {"rust": "1.98.0"}, {"cargo": "1.98.0"}, {"machine": "aarch64"},
                        {"label": "ubuntu-24.04"}, {"image": ""}):
            with self.subTest(changes=changes), self.assertRaises(baseline.BaselineError):
                self.check(**changes)

    def test_workflow_preserves_four_native_lanes_and_separate_evidence(self):
        root = Path(__file__).resolve().parents[1]
        workflow = (root / ".github/workflows/release-readiness.yml").read_text()
        package = workflow.split("  inventory:")[0]
        for label in ("ubuntu-22.04", "ubuntu-22.04-arm", "macos-15", "macos-15-intel"):
            self.assertIn(f"- os: {label}\n", package)
        for target in release.TARGETS:
            self.assertEqual(package.count(f"target: {target}"), 1)
        self.assertNotIn("ubuntu-24.04", package)
        self.assertIn("python-version: '3.12'", package)
        self.assertIn("rustup toolchain install 1.99.0", package)
        self.assertIn('cargo build --locked --release', package)
        self.assertIn("python3 scripts/linux_baseline.py", package)
        self.assertIn("if: always() && runner.os == 'Linux'", package)
        self.assertIn("name: loboot-linux-baseline-${{ matrix.target }}", package)
        self.assertIn("path: ${{ runner.temp }}/loboot-linux-baseline.json", package)
        self.assertIn("pattern: loboot-dist-*", workflow)
        self.assertIn('--allow-hosted-native --report "$EVIDENCE"', package)
        self.assertIn('if [ "$status" -eq 77 ]', package)
        self.assertNotIn("linux_baseline.py", (root / ".github/workflows/ci.yml").read_text())


if __name__ == "__main__":
    unittest.main()
