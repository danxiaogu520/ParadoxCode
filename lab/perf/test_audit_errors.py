"""Checks for evidence coverage and the review export's acceptance guards."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
import zipfile


SPEC = importlib.util.spec_from_file_location("audit_errors", Path(__file__).with_name("audit-errors.py"))
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class ErrorAuditTests(unittest.TestCase):
    def test_frozen_resources_require_same_corpus_and_exact_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, source = root / "game", root / "source"
            game.mkdir()
            source.mkdir()
            (game / "card.dds").write_bytes(b"")
            (source / "input.txt").write_text("texturefile = card.dds\n")
            diagnostic = {"severity": 1, "code": "UnknownTexturePath",
                          "range": {"start": {"line": 0, "character": 14}, "end": {"line": 0, "character": 22}},
                          "message": "texture file `card.dds` not found in any mod, game, or DLC pack root"}
            report = {"status": "passed", "tool_errors": [], "summary": {"errors": 1},
                      "inputs": {"source": str(source), "rules": {"manifest_rule_hash": "old", "source_format_version": 13}},
                      "files": [{"path": "input.txt", "physical_path": str(source / "input.txt"),
                                 "encoding": "utf-8", "diagnostics": [diagnostic]}]}
            old_report, new_report, manifest = root / "old.json", root / "new.json", root / "manifest.json"
            old_report.write_text(json.dumps(report))
            manifest.write_text(json.dumps({"rule_hash": "old", "source_format_version": 13}))
            previous, output = root / "previous", root / "output"
            with contextlib.redirect_stdout(io.StringIO()):
                AUDIT.export(old_report, manifest, game, previous, None)

            caches = [root / "old.db", root / "new.db"]
            for cache, rule_hash in zip(caches, ["old", "new"]):
                with sqlite3.connect(cache) as db:
                    db.execute("CREATE TABLE metadata (key TEXT, value TEXT)")
                    db.executemany("INSERT INTO metadata VALUES (?, ?)",
                                   [("rule_hash", rule_hash), ("source_root", str(source)),
                                    ("source_fingerprint", "same-corpus")])
            report["inputs"]["rules"]["manifest_rule_hash"] = "new"
            new_report.write_text(json.dumps(report))
            manifest.write_text(json.dumps({"rule_hash": "new", "source_format_version": 13}))
            (game / "card.dds").unlink()
            game.rmdir()

            def export():
                with contextlib.redirect_stdout(io.StringIO()):
                    AUDIT.export(new_report, manifest, None, output, None,
                                 prior_review=previous, prior_cache=caches[0], cache=caches[1])

            export()
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(summary["classifications"], {"corpus-missing-resource": 1})
            self.assertFalse(summary["resource_review"]["live_installation_checked"])

            diagnostic["message"] = "texture file `other.dds` not found in any mod, game, or DLC pack root"
            new_report.write_text(json.dumps(report))
            export()
            self.assertEqual(json.loads((output / "summary.json").read_text())["pending_errors"], 1)
            with sqlite3.connect(caches[1]) as db:
                db.execute("UPDATE metadata SET value = 'different' WHERE key = 'source_fingerprint'")
            with self.assertRaisesRegex(ValueError, "identical source_fingerprint"):
                export()
            with sqlite3.connect(caches[1]) as db:
                db.execute("UPDATE metadata SET value = 'same-corpus' WHERE key = 'source_fingerprint'")
                db.execute("UPDATE metadata SET value = 'different' WHERE key = 'rule_hash'")
            with self.assertRaisesRegex(ValueError, "manifest identities"):
                export()
            old_report.write_text("{}")
            with self.assertRaisesRegex(ValueError, "frozen identity"):
                export()

    def test_resource_evidence_and_duplicate_error_coverage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            game, source, output = root / "game", root / "source", root / "out"
            (game / "gfx").mkdir(parents=True)
            source.mkdir()
            (source / "input.txt").write_text("texturefile = gfx/card.tga\npicture = DLC_CARD\n")
            (game / "gfx/card.dds").write_bytes(b"")
            with zipfile.ZipFile(game / "dlc.zip", "w") as archive:
                archive.writestr("interface/card.gfx", 'spriteTypes = { spriteType = { name = "DLC_CARD" } }')
            manifest = {"rule_hash": "frozen", "source_format_version": 13}
            texture = {"severity": 1, "code": "UnknownTexturePath",
                       "range": {"start": {"line": 0, "character": 14}, "end": {"line": 0, "character": 26}},
                       "message": "texture file `gfx/card.tga` not found in any mod, game, or DLC pack root"}
            picture = {"severity": 1, "code": "InvalidValue", "range": texture["range"],
                       "message": "invalid value `dlc_card` for `picture`\nexpected a sprite name"}
            report = {"status": "passed", "tool_errors": [], "summary": {"errors": 3},
                      "inputs": {"source": str(source), "rules": {"manifest_rule_hash": "frozen", "source_format_version": 13}},
                      "files": [{"path": "input.txt", "physical_path": str(source / "input.txt"),
                                 "encoding": "utf-8", "diagnostics": [texture, texture, picture]}]}
            report_path, manifest_path = root / "report.json", root / "manifest.json"
            report_path.write_text(json.dumps(report))
            manifest_path.write_text(json.dumps(manifest))
            with contextlib.redirect_stdout(io.StringIO()):
                AUDIT.export(report_path, manifest_path, game, output, None)
            entries = json.loads((output / "errors.json").read_text())
            summary = json.loads((output / "summary.json").read_text())
            self.assertEqual(len({entry["id"] for entry in entries}), 3)
            self.assertEqual(summary["classifications"], {"corpus-missing-resource": 3})
            self.assertTrue(entries[0]["evidence"][0]["extension_fallback"])
            self.assertEqual(entries[2]["evidence"][0]["declaration"], "spriteType")

            # A name in a ZIP cannot establish corpus omission if that ZIP is
            # actually available to the audited source tree.
            (source / "dlc.zip").write_bytes((game / "dlc.zip").read_bytes())
            with contextlib.redirect_stdout(io.StringIO()):
                AUDIT.export(report_path, manifest_path, game, output, None)
            self.assertEqual(json.loads((output / "summary.json").read_text())["pending_errors"], 1)

            report["summary"]["errors"] = 4
            report_path.write_text(json.dumps(report))
            with self.assertRaisesRegex(ValueError, "error total"):
                AUDIT.export(report_path, manifest_path, game, output, None)
            report["summary"]["errors"] = 3
            report["inputs"]["rules"]["manifest_rule_hash"] = "different"
            report_path.write_text(json.dumps(report))
            with self.assertRaisesRegex(ValueError, "frozen manifest"):
                AUDIT.export(report_path, manifest_path, game, output, None)
            report["inputs"]["rules"]["manifest_rule_hash"] = "frozen"
            report_path.write_text(json.dumps(report))
            stale = root / "decisions.json"
            stale.write_text(json.dumps({"not-an-error": {}}))
            with self.assertRaisesRegex(ValueError, "stale or unmatched"):
                AUDIT.export(report_path, manifest_path, game, output, stale)


if __name__ == "__main__":
    unittest.main()
