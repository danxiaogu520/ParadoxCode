# ParadoxCode

[English](README.md) | [简体中文](README.zh-CN.md)

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/quick-ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/quick-ci.yml)
[![Release](https://img.shields.io/github/v/release/danxiaogu520/ParadoxCode)](https://github.com/danxiaogu520/ParadoxCode/releases)
[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/paradoxcode.paradoxcode-vscode)](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ParadoxCode is an independent, open-source language toolkit for Europa Universalis IV modding.
It combines a Rust language server with a VS Code extension. The analysis engine separates
shared language mechanisms from the EU4 rule package; EU4 is the supported game profile.

## What it does

- Diagnostics, completion, hover, definitions, references, and conflict-aware rename.
- Error-tolerant parsing, semantic highlighting, scope hints, and conservative formatting.
- Workspace analysis across your Mod, dependencies, and a local Vanilla index.
- Mission-tree preview and manual Chinese localisation encoding/decoding in VS Code;
  experimental transparent decoded views are available by opt-in.

## Start using it

Install [ParadoxCode from Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode).
The [VS Code guide](editors/vscode/README.md#setup) covers installation, first use, and troubleshooting.
For another editor, see the [language-server integration guide](crates/pdc/README.md).
MCP integration is maintained separately in the `ParadoxCodeMCP` project, which connects to this language server as an external process.

## Find the right reference

| I want to… | Start here |
| --- | --- |
| Configure the extension | [Generated settings and commands reference](editors/vscode/REFERENCE.md) |
| Understand a diagnostic | [Diagnostic guide](crates/ide/DIAGNOSTICS.md) |
| Build, test, or contribute | [Contributor guide](CONTRIBUTING.md) |
| Understand the architecture | [Architecture and crate dependencies](CONTRIBUTING.md#architecture) |
| Maintain EU4 rules | [Rule-package guide](rules/README.md) and [language semantics](crates/rules/LANGUAGE.md) |
| Run local corpus/performance audits | [Local experiment tools](lab/README.md) |
| Maintain or publish the project | [Governance](GOVERNANCE.md) and [release runbook](RELEASING.md) |

Current published versions are shown by the badges above and the
[GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases).
[CHANGELOG.md](CHANGELOG.md) distinguishes unreleased changes from published history.
CSV resources currently receive syntax-only handling. See the component guides for supported workflows.

ParadoxCode is **not affiliated with or endorsed by Paradox Interactive**. Europa Universalis IV
and Paradox Interactive are trademarks of their respective owners.
The source is [MIT licensed](LICENSE); game files, local caches, and external rule corpora are not redistributed.
Report vulnerabilities privately through [SECURITY.md](SECURITY.md).
