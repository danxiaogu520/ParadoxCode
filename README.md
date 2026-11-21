# ParadoxCode

[English](README.md) | [简体中文](README.zh-CN.md)

ParadoxCode is an independent, open-source language toolkit for Europa Universalis IV modding.
Its Rust language server and VS Code extension help you edit, navigate, and validate scripts
across your mod, dependencies, and base game files.

## Start using it

Install [ParadoxCode from Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode).
The [VS Code guide](editors/vscode/README.md#setup) covers installation, first use, and troubleshooting.
For another editor, see the [language-server integration guide](crates/pdc/README.md).

## Find the right reference

| I want to… | Start here |
| --- | --- |
| Configure the extension | [Generated settings and commands reference](editors/vscode/REFERENCE.md) |
| Use editor search | [VS Code search guide](editors/vscode/README.md#search) |
| Understand a diagnostic | [Diagnostic guide](crates/ide/DIAGNOSTICS.md) |
| Build, test, or contribute | [Contributor guide](CONTRIBUTING.md) |
| Understand the architecture | [Architecture and crate dependencies](CONTRIBUTING.md#architecture) |
| Maintain EU4 rules | [Rule-package guide](rules/README.md) and [language semantics](crates/rules/LANGUAGE.md) |
| Run local corpus/performance audits | [Local experiment tools](lab/README.md) |
| Download a release or see what changed | [GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases) and [changelog](CHANGELOG.md) |
| Maintain or publish the project | [Governance](GOVERNANCE.md) and [release runbook](RELEASING.md) |

ParadoxCode is **not affiliated with or endorsed by Paradox Interactive**. Europa Universalis IV
and Paradox Interactive are trademarks of their respective owners.
The source is [MIT licensed](LICENSE); game files, local caches, and external rule corpora are not redistributed.
Report vulnerabilities privately through [SECURITY.md](SECURITY.md).
