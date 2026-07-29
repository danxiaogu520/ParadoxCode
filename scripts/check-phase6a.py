#!/usr/bin/env python3
"""Validate the Phase 6A release asset and exercise a built pdx-ls process."""

from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import quote


ROOT = Path(__file__).resolve().parents[1]
RULES = ROOT / "rules" / "eu4.pdxrules"
MANIFEST = ROOT / "rules" / "manifest.json"
SERVER = ROOT / "target" / "debug" / "pdx-ls"
PACKAGER = ROOT / "scripts" / "package-server-release.py"


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def frame(value: dict) -> bytes:
    payload = json.dumps(value, separators=(",", ":")).encode()
    return b"Content-Length: " + str(len(payload)).encode() + b"\r\n\r\n" + payload


def decode_frames(data: bytes) -> list[dict]:
    result = []
    offset = 0
    while offset < len(data):
        separator = data.find(b"\r\n\r\n", offset)
        require(separator >= 0, "pdx-ls emitted an incomplete frame header")
        header = data[offset:separator].decode()
        length = next(
            int(line.split(":", 1)[1].strip())
            for line in header.split("\r\n")
            if line.lower().startswith("content-length:")
        )
        start = separator + 4
        result.append(json.loads(data[start : start + length]))
        offset = start + length
    return result


def file_uri(path: Path) -> str:
    return "file://" + quote(str(path), safe="/:@")


def check_artifact() -> None:
    require(RULES.is_file(), "rules/eu4.pdxrules is missing")
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    require(
        hashlib.sha256(RULES.read_bytes()).hexdigest() == manifest["artifact_sha256"],
        "rules artifact checksum mismatch",
    )
    with sqlite3.connect(RULES) as connection:
        connection.execute("PRAGMA foreign_keys = ON")
        schema_version = connection.execute(
            "SELECT value FROM metadata WHERE key = 'schema_version'"
        ).fetchone()[0]
        rule_hash = connection.execute(
            "SELECT value FROM metadata WHERE key = 'rule_hash'"
        ).fetchone()[0]
        game_id = connection.execute(
            "SELECT value FROM metadata WHERE key = 'game_id'"
        ).fetchone()[0]
        foreign_keys = connection.execute("PRAGMA foreign_keys").fetchone()[0]
    require(str(schema_version) == str(manifest["schema_version"]), "rules schema mismatch")
    require(rule_hash == manifest["rule_hash"], "rules rule_hash mismatch")
    require(game_id == manifest["game_id"] == "eu4", "rules game/profile mismatch")
    require(foreign_keys == 1, "rules artifact does not enable foreign keys")

    extension = (ROOT / "editors" / "zed" / "extension.toml").read_text(encoding="utf-8")
    require("[language_servers.pdx-ls]" in extension, "Zed language server is not registered")
    source = (ROOT / "editors" / "zed" / "src" / "lib.rs").read_text(encoding="utf-8")
    require("language_server_command" in source, "Zed language server command is not implemented")
    require("--rules" not in source, "Zed command still exposes a rules override")


def check_server_smoke() -> None:
    require(SERVER.is_file(), "pdx-ls binary is missing; run cargo build first")
    with tempfile.TemporaryDirectory(prefix="pdx-phase6a-smoke-") as directory:
        root = Path(directory)
        path = root / "common" / "events" / "smoke.txt"
        path.parent.mkdir(parents=True)
        path.write_text("country_event = { id = smoke.1 }\n", encoding="utf-8")
        uri = file_uri(path)
        messages = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {"rootUri": file_uri(root), "capabilities": {}},
            },
            {"jsonrpc": "2.0", "method": "initialized", "params": {}},
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": "eu4",
                        "version": 1,
                        "text": path.read_text(encoding="utf-8"),
                    }
                },
            },
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "textDocument/rename",
                "params": {
                    "textDocument": {"uri": uri},
                    "position": {"line": 0, "character": 25},
                    "newName": "smoke.2",
                },
            },
            {"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}},
            {"jsonrpc": "2.0", "method": "exit"},
        ]
        process = subprocess.run(
            [os.fspath(SERVER)],
            input=b"".join(frame(message) for message in messages),
            capture_output=True,
            check=False,
        )
        require(process.returncode == 0, f"pdx-ls smoke failed: {process.stderr.decode()}")
        responses = decode_frames(process.stdout)
        initialize = next(response for response in responses if response.get("id") == 1)
        require(initialize["result"]["capabilities"]["renameProvider"]["prepareProvider"], "rename capability missing")
        rename = next(response for response in responses if response.get("id") == 2)
        require("error" not in rename, f"rename smoke failed: {rename}")
        require(rename["result"]["changes"][uri][0]["newText"] == "smoke.2", "rename edit missing")


def check_packaged_server() -> None:
    require(SERVER.is_file(), "pdx-ls binary is missing; run cargo build first")
    with (ROOT / "Cargo.toml").open("rb") as handle:
        version = tomllib.load(handle)["workspace"]["package"]["version"]
    with tempfile.TemporaryDirectory(prefix="pdx-packaged-server-smoke-") as directory:
        destination = Path(directory)
        process = subprocess.run(
            [
                sys.executable,
                os.fspath(PACKAGER),
                "--version",
                version,
                "--target",
                "x86_64-unknown-linux-gnu",
                "--binary",
                os.fspath(SERVER),
                "--output-dir",
                os.fspath(destination),
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        require(process.returncode == 0, f"server packaging failed: {process.stderr}")
        archive_path = Path(process.stdout.strip())
        sidecar_path = archive_path.with_name(f"{archive_path.name}.sha256")
        digest, name = sidecar_path.read_text(encoding="utf-8").strip().split("  ", 1)
        require(name == archive_path.name, "packaged server checksum filename mismatch")
        require(
            digest == hashlib.sha256(archive_path.read_bytes()).hexdigest(),
            "packaged server checksum mismatch",
        )
        with tarfile.open(archive_path, "r:gz") as archive:
            members = archive.getmembers()
            require(
                len(members) == 1 and members[0].name == "pdx-ls" and members[0].mode == 0o755,
                "packaged server archive contract mismatch",
            )
            source = archive.extractfile(members[0])
            require(source is not None, "packaged server executable is missing")
            installed = destination / "installed-pdx-ls"
            installed.write_bytes(source.read())
        installed.chmod(0o755)
        version_result = subprocess.run(
            [os.fspath(installed), "--version"],
            check=False,
            capture_output=True,
            text=True,
        )
        require(version_result.returncode == 0, f"packaged pdx-ls failed: {version_result.stderr}")
        require(
            version_result.stdout.strip() == f"pdx-ls {version}",
            "packaged pdx-ls version output mismatch",
        )


def main() -> int:
    try:
        check_artifact()
        check_packaged_server()
        check_server_smoke()
        print("Phase 6A release asset, packaged server launch, and rename smoke passed.")
    except (OSError, KeyError, sqlite3.Error, RuntimeError, StopIteration, ValueError) as error:
        print(f"Phase 6A check failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
