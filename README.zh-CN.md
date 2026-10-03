# ParadoxCode

[English](README.md) | 简体中文

[![CI](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml/badge.svg)](https://github.com/danxiaogu520/ParadoxCode/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/danxiaogu520/ParadoxCode)](https://github.com/danxiaogu520/ParadoxCode/releases)
[![VS Code Marketplace](https://img.shields.io/visual-studio-marketplace/v/paradoxcode.paradoxcode-vscode)](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ParadoxCode 是面向《欧陆风云 IV》模组开发的独立开源语言工具，由 Rust 语言服务器和 VS Code 扩展组成。
分析引擎将通用语言机制与 EU4 规则包分开；当前支持的游戏是 EU4。

## 功能

- 诊断、补全、悬停、定义跳转、引用查询和冲突感知重命名。
- 容错解析、语义高亮、作用域提示和保守格式化。
- 联合分析当前模组、依赖模组与本地原版索引。
- VS Code 任务树预览与中文本地化透明转码。
- 面向 VS Code agent 和 MCP 客户端的只读查询及草稿校验。

## 开始使用

从 [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode) 安装 ParadoxCode。
安装、首次使用和故障排查见 [VS Code 使用指南](editors/vscode/README.md#setup)。
其他编辑器见 [语言服务器接入指南](crates/pdc/README.md)，agent 客户端见 [MCP 指南](crates/pdc/README.md#mcp-server)。

## 按问题查找

| 我想做什么 | 文档入口 |
| --- | --- |
| 配置扩展 | [自动生成的设置、命令和工具参考](editors/vscode/REFERENCE.md) |
| 理解诊断 | [诊断指南](crates/ide/DIAGNOSTICS.md) |
| 编译、测试或贡献 | [贡献指南](CONTRIBUTING.md) |
| 理解架构 | [架构与 crate 依赖](CONTRIBUTING.md#architecture) |
| 维护 EU4 规则 | [规则包指南](rules/README.md)与[语言语义](crates/rules/LANGUAGE.md) |
| 运行本地语料或性能审计 | [本地实验工具](lab/README.md) |
| 维护或发布项目 | [治理约定](GOVERNANCE.md)与[发布手册](RELEASING.md) |

已发布版本以徽章和 [GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases) 为准；
[CHANGELOG.md](CHANGELOG.md)区分未发布变化与版本历史。CSV 资源目前只做语法层处理，具体工作流以对应组件指南为准。

ParadoxCode **与 Paradox Interactive 无任何关联，也未获得其背书**。相关游戏与品牌商标归各自权利人所有。
源码采用 [MIT 许可证](LICENSE)，游戏文件、本地缓存与外部规则语料不随项目分发。
安全问题请按 [SECURITY.md](SECURITY.md)私下报告。
