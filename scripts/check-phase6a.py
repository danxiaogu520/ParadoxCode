#!/usr/bin/env python3
"""Validate the Phase 6A release asset and exercise a built pdx-ls process."""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import tempfile
from pathlib import Path
from urllib.parse import quote


ROOT = Path(__file__).resolve().parents[1]
RULES = ROOT / "rules" / "eu4.pdxrules"
BUNDLED_RULES = ROOT / "editors" / "zed" / "bundled-rules" / "eu4.pdxrules"
MANIFEST = ROOT / "rules" / "manifest.json"
SERVER = ROOT / "target" / "debug" / "pdx-ls"


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
    require(BUNDLED_RULES.is_file(), "Zed bundled rules artifact is missing")
    require(RULES.read_bytes() == BUNDLED_RULES.read_bytes(), "bundled rules differ from authority")
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
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
    require("bundled-rules/eu4.pdxrules" in source, "Zed command does not pass bundled rules")


def check_server_smoke() -> None:
    require(SERVER.is_file(), "pdx-ls binary is missing; run cargo build first")
    with tempfile.TemporaryDirectory(prefix="pdx-phase6a-smoke-") as directory:
        root = Path(directory)
        path = root / "common" / "events" / "smoke.txt"
        path.parent.mkdir(parents=True)
        path.write_text("country_event = { id = smoke.1 }\n", encoding="utf-8")
        uri = file_uri(path)
        messages = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": file_uri(root)}},
            {"jsonrpc": "2.0", "method": "initialized", "params": {}},
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {"textDocument": {"uri": uri, "version": 1, "text": path.read_text(encoding="utf-8")}},
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
            [os.fspath(SERVER), "--rules", os.fspath(BUNDLED_RULES)],
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


def main() -> int:
    try:
        check_artifact()
        check_server_smoke()
        print("Phase 6A release asset, server launch, and rename smoke passed.")
    except (OSError, KeyError, sqlite3.Error, RuntimeError, StopIteration) as error:
        print(f"Phase 6A check failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
