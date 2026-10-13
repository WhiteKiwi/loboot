import copy
import unittest

import render_homebrew
from release import ReleaseError, SCHEMA, TARGETS, archive_stem


class FormulaTests(unittest.TestCase):
    def fixture(self):
        return {"schema_version": SCHEMA, "version": "0.1.0", "artifacts": [
            {"schema_version": SCHEMA, "version": "0.1.0", "target": target,
             "archive": f"{archive_stem('0.1.0', target)}.tar.gz", "sha256": str(i) * 64}
            for i, target in enumerate(TARGETS)]}

    def test_exact_formula_and_no_service(self):
        formula = render_homebrew.render(self.fixture())
        self.assertEqual(formula.count('      sha256 "'), 4)
        self.assertEqual(formula.count('      url "https://github.com/WhiteKiwi/loboot/releases/download/v0.1.0/'), 4)
        self.assertIn('license any_of: ["MIT", "Apache-2.0"]', formula)
        self.assertIn('refute_path_exists root', formula)
        self.assertNotIn('service do', formula)
        self.assertNotIn('post_install', formula)

    def test_version_is_inferred_from_all_archive_urls(self):
        for version in ("0.1.0", "1.2.3"):
            manifest = self.fixture()
            manifest["version"] = version
            for item in manifest["artifacts"]:
                item["version"] = version
                item["archive"] = f"{archive_stem(version, item['target'])}.tar.gz"
            with self.subTest(version=version):
                formula = render_homebrew.render(manifest)
                self.assertNotRegex(formula, r'(?m)^\s*version\s+"')
                self.assertEqual(formula.count(f"/releases/download/v{version}/loboot-v{version}-"), 4)

    def test_refuse_missing_duplicate_extra_or_injected_metadata(self):
        for field, value in (("target", "../oops"), ("archive", "../oops"), ("version", "0.2.0"),
                             ("sha256", "x" * 64), ("schema_version", "unknown")):
            manifest = self.fixture()
            manifest["artifacts"][0][field] = value
            with self.subTest(field=field), self.assertRaises(ReleaseError):
                render_homebrew.render(manifest)
        manifest = self.fixture()
        manifest["artifacts"][0] = copy.deepcopy(manifest["artifacts"][1])
        with self.assertRaises(ReleaseError): render_homebrew.render(manifest)
        for count in (0, 1, 3, 5):
            manifest = self.fixture()
            manifest["artifacts"] = (manifest["artifacts"] * 2)[:count]
            with self.subTest(count=count), self.assertRaises(ReleaseError): render_homebrew.render(manifest)
