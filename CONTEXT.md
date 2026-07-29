# ParadoxCode Domain Language

ParadoxCode 使用统一术语描述 PDX 文本分析与编辑行为，避免产品偏好和实现细节混为一谈。

## Language

**规范格式化（Canonical Formatting）**:
在不改变文档语义内容的边界内，将所有可安全格式化的等价输入收敛为唯一、稳定的布局。
_Avoid_: 美化、最小改动格式化

**格式化安全门（Formatting Safety Gate）**:
formatter 无法证明布局重写安全时产生的无编辑结果；它与 unknown key、unknown symbol、scope error 等语义诊断无关。
_Avoid_: 局部容错格式化、尽力格式化

**规范缩进（Canonical Indentation）**:
以真实 tab 表示嵌套层级、并以四列作为 tab 视觉宽度的项目缩进语言。
_Avoid_: 软 tab、客户端决定的缩进

**规范块布局（Canonical Block Layout）**:
根据 block 内容分类得到的唯一单行或展开形态。
_Avoid_: 保留原换行、仅行内空白格式化

**空 Block（Empty Block）**:
不包含语义 item 或 comment 的 block。
_Avoid_: `{}`、展开的空 block

**纯 Scalar Block（Scalar-only Block）**:
只包含 bare 或 opaque quoted scalar，不包含 property、嵌套 block 或 comment 的 block。
_Avoid_: list、简单 block

**直接 Property（Direct Property）**:
以某个 block 为直接父级的 property，不包括更深层嵌套 block 中的 property。
_Avoid_: 所有后代 property

**Quoted Script**:
PdxScript 文件中原本含有换行、且 decoded payload 能完整解释为包含 property、header block 或 parameter block 的 PdxScript quoted scalar。
_Avoid_: 普通 quoted scalar、localisation value

**Block Header Comment**:
block 内位于所有语义 item 之前的第一个 leading comment。
_Avoid_: block 内首行注释、普通独立注释

**布局空行（Layout Blank Line）**:
可格式化语法布局中的空白行；opaque quoted scalar 内部的空白行属于 scalar 内容，不是布局空行。
_Avoid_: 分组空行、最多一个空行
