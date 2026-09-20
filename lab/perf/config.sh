# lab/perf 配置（本文件入库，只放可移植默认值；机器本地路径写同目录 config.local.sh——被 ignore）。
# 被 perf.sh source；同名环境变量优先于这里的默认值。

# --- ParadoxCode 仓库（脚本自动定位，一般无需改） ---
REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

# --- 本地语料（data/ 整体被 gitignore，绝不入库或上传） ---
# data/vanilla 是 EU4 文本游戏数据的本地副本（只含 LSP 会解析的文本类别，
# 不含 exe/dll/图片音频等二进制）。有了它，实验组（sweep）与对照组（cwtools native）
# 在同一操作系统、同一文件系统（ext4）、同一棵语料树上对比，才是公平比较。
DATA_DIR="${DATA_DIR:-$REPO_ROOT/data}"
PDC_VANILLA_COPY="${PDC_VANILLA_COPY:-$DATA_DIR/vanilla}"
EDG_DIR="${EDG_DIR:-$DATA_DIR/mods/entrance_to_the_desolate_ground}"

# 实验组语料解析顺序：环境变量 PDC_VANILLA_SOURCE > data/vanilla 副本 > ParadoxCode
# 用户配置 > 标准 Steam 安装路径。优先本地副本（快且公平），没有再退回原安装。
if [ -z "${PDC_VANILLA_SOURCE:-}" ]; then
  if [ -d "$PDC_VANILLA_COPY" ]; then
    PDC_VANILLA_SOURCE="$PDC_VANILLA_COPY"
  else
    _default_vanilla="$(sed -n "s/^vanilla_source *= *'\(.*\)'/\1/p" "${XDG_CONFIG_HOME:-$HOME/.config}/paradoxcode/config.toml" 2>/dev/null | head -1)"
    PDC_VANILLA_SOURCE="${_default_vanilla:-/mnt/c/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV}"
  fi
fi

# EDG（归墟之门）语料来源：标准 Steam workshop 位置（EU4 appid 236850，EDG 模组 ID 3047072888）
EDG_WORKSHOP_SOURCE="${EDG_WORKSHOP_SOURCE:-/mnt/c/Program Files (x86)/Steam/steamapps/workshop/content/236850/3047072888}"

# --- 搬运过滤器（与 crates/game/src/eu4/mod.rs 的权威分类一致；.mod 为模组描述符） ---
CORPUS_EXTENSIONS="${CORPUS_EXTENSIONS:-txt TXT gui gfx asset sfx json lua yml yaml mod}"

# ParadoxCode 服务端用户缓存根（冷跑前需手动移除其中的 .pdcindex）
PDC_CACHE_ROOT="${PDC_CACHE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/paradoxcode}"
PDC_GAME_ID="${PDC_GAME_ID:-eu4}"

# --- 对照组：cwtools-rs ---
# native（默认，公平模式）：Linux 构建的 cwtools，二进制收在本 lab 的 bin/ 下（ignored）。
CWTOOLS_NATIVE_BIN="${CWTOOLS_NATIVE_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bin/cwtools}"
# 构建来源仓库与规则目录属机器本地信息，放 config.local.sh，例如：
#   CWTOOLS_REPO=/path/to/cwtools-rs          # native 构建来源（checkout 完整仓库）
#   CWTOOLS_RULES=/path/to/cwtools-eu4-config # EU4 .cwt 规则目录
# native 构建的独立 target 目录（不写来源仓库）
CWTOOLS_NATIVE_TARGET="${CWTOOLS_NATIVE_TARGET:-$HOME/.cache/paradoxcode-perf/cwtools-target}"
CWTOOLS_GAME="${CWTOOLS_GAME:-eu4}"

# --- 采样纪律 ---
CONTROL_RUNS="${CONTROL_RUNS:-5}"        # 对照组 hyperfine 计时次数
CONTROL_WARMUP="${CONTROL_WARMUP:-1}"    # 对照组不计时的预热次数
NOISE_PCT="${NOISE_PCT:-3}"              # |Δ%| 低于此值视为噪声区间（标记 ≈）
BENCH_REPEAT="${BENCH_REPEAT:-1}"        # 基准重复次数；>1 时每指标取中位数（稳定模式）

# --- 画像工具优先级（samply > perf > 报错并给安装提示） ---
SAMPLY_BIN="${SAMPLY_BIN:-$(command -v samply || true)}"
# /usr/bin/perf 是按内核版本分发的包装脚本，WSL2 内核没有对应包会拒跑；
# 真正可用的实体二进制在 /usr/lib/linux-tools/<ver>/perf，优先取它。
_perf_lib="$(ls -1 /usr/lib/linux-tools/*/perf 2>/dev/null | sort -V | tail -1 || true)"
if [ -n "$_perf_lib" ]; then
  PERF_BIN="${PERF_BIN:-$_perf_lib}"
else
  PERF_BIN="${PERF_BIN:-$(command -v perf || true)}"
fi

# --- 机器本地覆盖（被 ignore，不入库） ---
# shellcheck disable=SC1091
[ -f "$(dirname "${BASH_SOURCE[0]}")/config.local.sh" ] && . "$(dirname "${BASH_SOURCE[0]}")/config.local.sh"
