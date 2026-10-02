#!/usr/bin/env python3
"""Inventory every Vanilla error, with evidence and explicit unresolved items.

This is independent of the legacy diagnostic baseline. Corpus-derived entries,
snippets and evidence stay in the ignored performance-results directory.
"""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import zipfile


ROOT = Path(__file__).resolve().parents[2]


def normalize_path(value):
    return re.sub(r"/+", "/", value.replace("\\", "/")).lstrip("/")


def read_text(path):
    raw = path.read_bytes()
    try:
        return raw.decode("utf-8-sig")
    except UnicodeDecodeError:
        return raw.decode("cp1252")


def resource_inventory(installation):
    """Read names and GFX declarations only; never extract binary assets."""
    files = defaultdict(list)
    sprites = defaultdict(list)
    archive_errors = []
    sprite_pattern = re.compile(
        r'\bspriteType\s*=\s*\{[^{}]*?\bname\s*=\s*(?:"([^"\r\n]+)"|([^\s{}#]+))'
    )

    def collect_sprites(text, origin):
        # Match direct names in spriteType blocks, not arbitrary `name` fields.
        text = "\n".join(line.split("#", 1)[0] for line in text.splitlines())
        for match in sprite_pattern.finditer(text):
            name = match.group(1) or match.group(2)
            number = text.count("\n", 0, match.start()) + 1
            sprites[name.lower()].append({"source": origin, "line": number, "name": name,
                                          "declaration": "spriteType"})

    for path in installation.rglob("*"):
        if not path.is_file():
            continue
        relative = path.relative_to(installation).as_posix()
        files[relative.lower()].append({"kind": "loose", "path": relative})
        if path.suffix.lower() == ".gfx":
            collect_sprites(read_text(path), relative)
        elif path.suffix.lower() == ".zip":
            try:
                with zipfile.ZipFile(path) as archive:
                    for item in archive.infolist():
                        if item.is_dir():
                            continue
                        member = normalize_path(item.filename)
                        origin = {"kind": "archive", "path": member, "archive": relative}
                        files[member.lower()].append(origin)
                        if member.lower().endswith(".gfx"):
                            raw = archive.read(item)
                            try:
                                text = raw.decode("utf-8-sig")
                            except UnicodeDecodeError:
                                text = raw.decode("cp1252")
                            collect_sprites(text, f"{relative}!{member}")
            except (OSError, UnicodeError, zipfile.BadZipFile) as error:
                archive_errors.append({"archive": relative, "error": str(error)})
    return files, sprites, archive_errors


def group_message(message):
    head = message.split("\n", 1)[0]
    head = re.sub(r"^texture file `[^`]*`", "texture file VALUE", head)
    head = re.sub(r"^invalid value `[^`]*`", "invalid value VALUE", head)
    head = re.sub(r"^argument `[^`]*`", "argument VALUE", head)
    return re.sub(r"renders as `[^`]*`", "renders as VALUE", head)


def frozen_resources(prior_review, prior_cache, cache, manifest, report):
    """Reuse only exact diagnostic identities from a verified identical corpus."""
    def metadata(path):
        with sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True) as db:
            return {key: value.decode() if isinstance(value, bytes) else value
                    for key, value in db.execute("SELECT key, value FROM metadata")}

    before, after = metadata(prior_cache), metadata(cache)
    summary_path, errors_path = prior_review / "summary.json", prior_review / "errors.json"
    summary = json.loads(summary_path.read_text())
    prior_report = Path(summary["report"])
    if hashlib.sha256(prior_report.read_bytes()).hexdigest() != summary["report_sha256"]:
        raise ValueError("prior resource report differs from its frozen identity")
    if before["rule_hash"] != summary["manifest"]["rule_hash"] or after["rule_hash"] != manifest["rule_hash"]:
        raise ValueError("resource review cache and manifest identities differ")
    if (Path(report["inputs"]["source"]).resolve() != Path(after["source_root"]).resolve()
            or Path(json.loads(prior_report.read_text())["inputs"]["source"]).resolve() != Path(before["source_root"]).resolve()):
        raise ValueError("resource review report and cache source roots differ")
    for key in ("source_fingerprint", "source_root"):
        if not before.get(key) or before[key] != after.get(key):
            raise ValueError(f"resource review requires identical {key}")
    entries = {entry["id"]: entry for entry in json.loads(errors_path.read_text())
               if entry["classification"] in ("corpus-missing-resource", "missing-resource-review",
                                               "resource-resolution-review", "sprite-declaration-review")}
    provenance = {"mode": "historical-exact-diagnostic-identities",
                  "source_fingerprint": after["source_fingerprint"],
                  "source_review": str(prior_review.resolve()),
                  "summary_sha256": hashlib.sha256(summary_path.read_bytes()).hexdigest(),
                  "errors_sha256": hashlib.sha256(errors_path.read_bytes()).hexdigest(),
                  "live_installation_checked": False}
    return entries, summary, provenance


def export(report_path, manifest_path, installation, output, decisions, *,
           prior_review=None, prior_cache=None, cache=None):
    report = json.loads(report_path.read_text())
    manifest = json.loads(manifest_path.read_text())
    if report["status"] != "passed" or report["tool_errors"]:
        raise ValueError("audit incomplete or contains tool errors")
    if report["inputs"]["rules"]["manifest_rule_hash"] != manifest["rule_hash"]:
        raise ValueError("report and frozen manifest differ")
    if report["inputs"]["rules"]["source_format_version"] != manifest["source_format_version"]:
        raise ValueError("report and manifest source versions differ")
    inherited, resource_review = {}, {"mode": "live-installation", "live_installation_checked": True}
    if prior_review:
        inherited, previous, resource_review = frozen_resources(prior_review, prior_cache, cache, manifest, report)
        resources, sprites = {}, {}
        archive_errors = previous["archive_errors"]
        installation_path = previous["installation"]
    else:
        resources, sprites, archive_errors = resource_inventory(installation)
        installation_path = str(installation.resolve())
    decisions = json.loads(decisions.read_text()) if decisions else {}
    errors = []
    occurrences = Counter()
    used_decisions = set()
    source = Path(report["inputs"]["source"])
    for file in report["files"]:
        diagnostics = [d for d in file["diagnostics"] if d["severity"] == 1]
        if not diagnostics:
            continue
        lines = Path(file["physical_path"]).read_bytes().decode(file["encoding"]).splitlines()
        for diagnostic in diagnostics:
            identity = json.dumps([file["path"], diagnostic["code"], diagnostic["range"],
                                   diagnostic["message"]], ensure_ascii=False, sort_keys=True)
            occurrences[identity] += 1
            error_id = hashlib.sha256(f"{identity}:{occurrences[identity]}".encode()).hexdigest()
            line = diagnostic["range"]["start"]["line"]
            entry = {"id": error_id, "path": file["path"], **diagnostic,
                     "group": group_message(diagnostic["message"]),
                     "context": [{"line": i + 1, "text": lines[i]}
                                 for i in range(max(0, line - 3), min(len(lines), line + 4))],
                     "classification": "unresolved", "explanation": None, "evidence": []}
            texture = re.fullmatch(r"texture file `([^`]*)` not found in any mod, game, or DLC pack root",
                                   diagnostic["message"])
            picture = re.match(r"invalid value `([^`]*)` for `picture`", diagnostic["message"])
            if texture:
                path = normalize_path(texture.group(1))
                found = resources.get(path.lower(), [])
                resolved_path = path
                if not found and Path(path).suffix.lower() in (".tga", ".dds"):
                    suffix = ".dds" if Path(path).suffix.lower() == ".tga" else ".tga"
                    resolved_path = str(Path(path).with_suffix(suffix))
                    found = resources.get(resolved_path.lower(), [])
                    if found:
                        found = [dict(origin, extension_fallback=True,
                                      resolver="crates/engine/src/texture.rs::extension_fallback")
                                 for origin in found]
                if found and not (source / resolved_path).is_file():
                    entry.update(classification="corpus-missing-resource",
                                 explanation="The text corpus omits an asset present in the installed game or a DLC archive.",
                                 evidence=found)
                elif found:
                    entry.update(classification="resource-resolution-review",
                                 explanation="The installation contains this asset; inspect path case and corpus/VFS resolution.",
                                 evidence=found)
                else:
                    entry.update(classification="missing-resource-review",
                                 explanation=("No resource evidence for this diagnostic exists in the frozen review; the installation was not rechecked."
                                              if prior_review else
                                              "No matching installed loose file or archive member was found; this alone does not prove a Vanilla bug."))
            elif picture:
                name = picture.group(1)
                found = sprites.get(name.lower(), []) + sprites.get(f"GFX_{name}".lower(), [])
                if found:
                    entry.update(classification="sprite-declaration-review",
                                 explanation="The installation has a matching GFX name; inspect the declaration and DLC/source inventory before accepting it.",
                                 evidence=found)
                    if all("!" in origin["source"] and
                           not (source / origin["source"].split("!", 1)[0]).is_file()
                           for origin in found):
                        entry.update(classification="corpus-missing-resource",
                                     explanation="The event picture has a direct spriteType declaration inside an installed DLC archive; the text corpus excludes that archive.")
            if error_id in inherited:
                previous_entry = inherited[error_id]
                entry.update({key: previous_entry[key] for key in ("classification", "explanation", "evidence")})
                entry["resource_evidence_origin"] = str(prior_review.resolve())
            if error_id in decisions:
                decision = decisions[error_id]
                if decision.get("classification") not in ("vanilla-bug", "rule-bug", "analyzer-bug", "valid-diagnostic", "corpus-missing-resource"):
                    raise ValueError(f"invalid decision classification: {error_id}")
                if not decision.get("explanation") or not decision.get("evidence"):
                    raise ValueError(f"decision requires explanation and evidence: {error_id}")
                entry.update({key: decision[key] for key in ("classification", "explanation", "evidence")})
                used_decisions.add(error_id)
            errors.append(entry)
    if len(errors) != report["summary"]["errors"]:
        raise ValueError("inventory does not match the full report error total")
    if set(decisions) != used_decisions:
        raise ValueError("decisions contain stale or unmatched diagnostic identities")
    groups = defaultdict(list)
    for entry in errors:
        groups[(entry["code"], entry["group"], entry["classification"])].append(entry["id"])
    counts = dict(Counter(entry["classification"] for entry in errors))
    resolved = {"vanilla-bug", "valid-diagnostic", "corpus-missing-resource"}
    pending = sum(count for kind, count in counts.items() if kind not in resolved)
    summary = {"status": "explained" if not pending and not archive_errors else "semantic-review-required",
               "criterion": "Every error explained by evidence; legacy equality is not required.",
               "report": str(report_path.resolve()),
               "report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(), "manifest": manifest,
               "installation": installation_path, "resource_review": resource_review,
               "files_diagnosed": len(report["files"]),
               "total_errors": len(errors), "by_code": dict(Counter(e["code"] for e in errors)),
               "classifications": counts, "pending_errors": pending,
               "archive_errors": archive_errors,
               "groups": [{"code": code, "message": message, "classification": kind,
                           "count": len(ids), "error_ids": ids}
                          for (code, message, kind), ids in sorted(groups.items(), key=lambda x: -len(x[1]))]}
    output.mkdir(parents=True, exist_ok=True)
    for name, document in [("errors.json", errors), ("summary.json", summary)]:
        (output / name).write_text(json.dumps(document, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: summary[key] for key in ("status", "total_errors", "classifications", "pending_errors", "by_code")}, ensure_ascii=False))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    resources = parser.add_mutually_exclusive_group(required=True)
    resources.add_argument("--installation", type=Path)
    resources.add_argument("--prior-review", type=Path, help="frozen resource review for the identical text corpus")
    parser.add_argument("--prior-cache", type=Path)
    parser.add_argument("--cache", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--decisions", type=Path)
    args = parser.parse_args()
    if not args.output.resolve().is_relative_to(ROOT / "performance-results"):
        parser.error("corpus-derived output must stay under performance-results/")
    if args.installation and not args.installation.is_dir():
        parser.error("installation is not a directory")
    if args.prior_review and (not args.prior_cache or not args.cache):
        parser.error("frozen resource reuse requires --prior-cache and --cache")
    export(args.report, args.manifest, args.installation, args.output, args.decisions,
           prior_review=args.prior_review, prior_cache=args.prior_cache, cache=args.cache)


if __name__ == "__main__":
    main()
