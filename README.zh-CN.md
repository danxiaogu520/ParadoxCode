# ParadoxCode

[English](README.md) | 简体中文

ParadoxCode 是面向《欧陆风云 IV》模组开发的独立开源语言工具。
它由 Rust 语言服务器和 VS Code 扩展组成，支持脚本编辑、代码导航和诊断，可联合分析模组、依赖与原版游戏文件。

## 开始使用

从 [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=paradoxcode.paradoxcode-vscode) 安装 ParadoxCode。
安装、首次使用和故障排查见 [VS Code 使用指南](editors/vscode/README.md#setup)。
其他编辑器见 [语言服务器接入指南](crates/pdc/README.md)。

## 按问题查找

| 我想做什么 | 文档入口 |
| --- | --- |
| 配置扩展 | [自动生成的设置和命令参考](editors/vscode/REFERENCE.md) |
| 使用编辑器搜索 | [VS Code 搜索指南](editors/vscode/README.md#search) |
| 理解诊断 | [诊断指南](crates/ide/DIAGNOSTICS.md) |
| 编译、测试或贡献 | [贡献指南](CONTRIBUTING.md) |
| 理解架构 | [架构与 crate 依赖](CONTRIBUTING.md#architecture) |
| 维护 EU4 规则 | [规则包指南](rules/README.md)与[语言语义](crates/rules/LANGUAGE.md) |
| 运行本地语料或性能审计 | [本地实验工具](lab/README.md) |
| 下载发行版或查看变更 | [GitHub Releases](https://github.com/danxiaogu520/ParadoxCode/releases) 与 [变更日志](CHANGELOG.md) |
| 维护或发布项目 | [治理约定](GOVERNANCE.md)与[发布手册](RELEASING.md) |

ParadoxCode **与 Paradox Interactive 无任何关联，也未获得其背书**。相关游戏与品牌商标归各自权利人所有。
源码采用 [MIT 许可证](LICENSE)，游戏文件、本地缓存与外部规则语料不随项目分发。
安全问题请按 [SECURITY.md](SECURITY.md)私下报告。
