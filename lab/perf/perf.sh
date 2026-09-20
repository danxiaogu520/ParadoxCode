#!/usr/bin/env bash
#
# ParadoxCode 性能优化循环工作流（lab/perf，脚本入库；运行产物全部 gitignore）。
#
#   ./perf.sh init                    环境自检（--install 顺带安装缺失的系统依赖）
#   ./perf.sh import-corpus what      搬运语料到 data/：what = vanilla | edg | all（--prune 同步删除）
#   ./perf.sh status                  语料 / 对照组二进制 / 当前基线 / 最近运行
#   ./perf.sh bench [--repeat N] [-- 过滤参数]
#                                     跑仓库基准；N>1 时每指标取中位数（稳定模式）
#   ./perf.sh baseline <name>         建基线：构建 + 基准 + 全量 sweep，存入 baselines/<name>/
#   ./perf.sh sweep [--label X]       单独跑一次全量 sweep（--previous 传基线 summary 可看漂移）
#   ./perf.sh ab [baseline]           A/B：当前工作区 vs 基线（基准指标 + sweep 相位 + 诊断漂移）
#   ./perf.sh control [--runs N]      对照组：cwtools-rs（Linux native）跑同一语料；
#                                     --build 强制重建二进制；--timings-only 只采样相位
#   ./perf.sh profile <目标>          采样画像：bench:<名称> 或 sweep（samply / perf 自动选择）
#
# 公平性设计：实验组（sweep）与对照组（native cwtools）都在本机 Linux 上读同一份
# data/ 语料副本（ext4），同 OS、同文件系统、同语料。
# 位置与约定见同目录 README.md。结果只落 runs/ baselines/ profiles/（gitignore），绝不进 CI/PR。

set -euo pipefail

LAB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=config.sh
. "$LAB/config.sh"

RUNS_DIR="$LAB/runs"
BASELINES_DIR="$LAB/baselines"
mkdir -p "$RUNS_DIR" "$BASELINES_DIR"

# ----------------------------------------------------------------------------- 助手

# 进度/结果消息一律走 stderr：stdout 专供被 $(...) 捕获的返回值（路径、计数）
c_info() { local fmt="$1"; shift; printf '\033[1;34m[perf]\033[0m %s\n' "$(printf "$fmt" "$@")" >&2; }
c_ok()   { local fmt="$1"; shift; printf '\033[1;32m[perf]\033[0m %s\n' "$(printf "$fmt" "$@")" >&2; }
c_warn() { local fmt="$1"; shift; printf '\033[1;33m[perf]\033[0m %s\n' "$(printf "$fmt" "$@")" >&2; }
die()    { local fmt="$1"; shift; printf '\033[1;31m[perf] 错误:\033[0m %s\n' "$(printf "$fmt" "$@")" >&2; exit 1; }

new_run_dir() { # $1 = 类型标签
  local dir="$RUNS_DIR/$(date +%Y%m%d-%H%M%S)-$1-$$"
  mkdir -p "$dir"
  echo "$dir"
}

git_state_json() {
  jq -n --arg rev "$(git -C "$REPO_ROOT" rev-parse HEAD)" \
        --arg branch "$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD)" \
        --argjson dirty "$(git -C "$REPO_ROOT" status --porcelain | head -1 | grep -q . && echo true || echo false)" \
    '{rev: $rev, branch: $branch, dirty: $dirty}'
}

rule_hash() { jq -r '.rule_hash' "$REPO_ROOT/rules/manifest.json"; }

# vanilla 原始安装位置（搬运来源；与 PDC_VANILLA_SOURCE 不同，后者可能已指向 data/ 副本）
vanilla_origin() {
  if [ -n "${PDC_VANILLA_ORIGIN:-}" ]; then echo "$PDC_VANILLA_ORIGIN"; return; fi
  local from_config
  from_config="$(sed -n "s/^vanilla_source *= *'\(.*\)'/\1/p" "${XDG_CONFIG_HOME:-$HOME/.config}/paradoxcode/config.toml" 2>/dev/null | head -1)"
  echo "${from_config:-/mnt/c/Program Files (x86)/Steam/steamapps/common/Europa Universalis IV}"
}

build_server() { # 构建并回显 release 二进制路径
  c_info "构建 release 服务端（cargo build --locked --release -p pdc --bin paradoxcode）…"
  cargo build --locked --release --manifest-path "$REPO_ROOT/Cargo.toml" -p pdc --bin paradoxcode
  local bin="$REPO_ROOT/target/release/paradoxcode"
  [ -x "$bin" ] || die "构建后未找到 $bin"
  echo "$bin"
}

build_control_native() { # 从 CWTOOLS_REPO 构建 Linux cwtools 并装入 lab/perf/bin
  [ -n "${CWTOOLS_REPO:-}" ] && [ -d "$CWTOOLS_REPO" ] \
    || die "未配置对照组来源仓库 CWTOOLS_REPO（写在 lab/perf/config.local.sh，见 config.sh 注释）"
  c_info "构建 native 对照组（target 独立在 %s，不写来源仓库）…" "$CWTOOLS_NATIVE_TARGET"
  CARGO_TARGET_DIR="$CWTOOLS_NATIVE_TARGET" \
    cargo build --locked --release --manifest-path "$CWTOOLS_REPO/Cargo.toml" -p cwtools_cli
  mkdir -p "$(dirname "$CWTOOLS_NATIVE_BIN")"
  cp "$CWTOOLS_NATIVE_TARGET/release/cwtools" "$CWTOOLS_NATIVE_BIN"
  c_ok "对照组二进制: %s (%s, sha256 %s)" \
    "$CWTOOLS_NATIVE_BIN" "$("$CWTOOLS_NATIVE_BIN" --version)" \
    "$(sha256sum "$CWTOOLS_NATIVE_BIN" | cut -c1-16)"
}

# 解析 cargo bench 输出为 TSV：bench <TAB> 指标 <TAB> 毫秒
parse_bench_tsv() { # $1 = bench 原始输出, $2 = 输出 tsv
  awk '
    /Running benches\// {
      bench = $0
      sub(/^.*Running benches\//, "", bench)
      sub(/\.rs.*$/, "", bench)
      next
    }
    {
      line = $0
      if (match(line, /^[ \t]*[^ \t].*:[ \t]+[0-9]+(\.[0-9]+)? ms[ \t]*$/)) {
        label = line
        sub(/:[ \t]+[0-9]+(\.[0-9]+)? ms[ \t]*$/, "", label)
        gsub(/^[ \t]+|[ \t]+$/, "", label)
        value = line
        sub(/^.*:[ \t]+/, "", value)
        sub(/ ms.*$/, "", value)
        if (bench != "") printf "%s\t%s\t%s\n", bench, label, value
      }
    }
  ' "$1" > "$2"
}

# 多份同构 TSV 按键取中位数（偶数取下中位）
aggregate_median_tsv() { # $1 = 输出, 其余 = 输入 tsv…
  local out="$1"; shift
  cat "$@" | sort -t$'\t' -k1,1 -k2,2 -k3,3g | awk -F'\t' '
    { key = $1 "|" $2; count[key]++; rows[key " " count[key]] = $0 }
    END {
      for (k in count) print rows[k " " int((count[k] + 1) / 2)]
    }
  ' | sort -t$'\t' -k1,1 -k2,2 > "$out"
}

# 跑基准套件：1 遍预热 + repeat 遍记录；repeat>1 时 bench.tsv 取中位数，原始各遍在 bench-runs/
# 用法: run_bench_suite <目标目录> <repeat> [cargo bench 额外参数...]
run_bench_suite() {
  local dir="$1" repeat="$2"; shift 2
  local extra=("$@")
  [ ${#extra[@]} -eq 0 ] && extra=(--locked --workspace --all-features --benches)
  mkdir -p "$dir/bench-runs"
  c_info "基准预热（首遍，不记录）…"
  (cd "$REPO_ROOT" && cargo bench "${extra[@]}") > "$dir/bench-warmup.txt" 2>&1
  local i
  for i in $(seq 1 "$repeat"); do
    c_info "记录基准第 %d/%d 遍…" "$i" "$repeat"
    (cd "$REPO_ROOT" && cargo bench "${extra[@]}") 2>&1 | tee "$dir/bench-runs/pass-$i.txt" >&2
    parse_bench_tsv "$dir/bench-runs/pass-$i.txt" "$dir/bench-runs/pass-$i.tsv"
  done
  if [ "$repeat" -gt 1 ]; then
    aggregate_median_tsv "$dir/bench.tsv" "$dir"/bench-runs/pass-*.tsv
    c_ok "稳定模式：%d 遍，每指标取中位数" "$repeat"
  else
    cp "$dir/bench-runs/pass-1.tsv" "$dir/bench.tsv"
  fi
  c_ok "基准指标 %d 行" "$(wc -l < "$dir/bench.tsv")"
}

# 对比两份三列 TSV（键 = 前两列），输出 markdown 表；|Δ%| ≤ noise 标 ≈，否则 !
compare_tsv_md() { # $1 旧 $2 新 $3 noise% $4 表头(格式: 键1;键2;旧;新;Δ%)
  local header="$4"
  awk -F'\t' -v noise="$3" '
    NR == FNR { old[$1 "|" $2] = $3; next }
    { new[$1 "|" $2] = $3 }
    END {
      for (k in new) {
        if (k in old && old[k] + 0 > 0) {
          split(k, p, "|")
          d = (new[k] - old[k]) / old[k] * 100
          printf "%s\t%s\t%s\t%s\t%+.1f\t%s\n", p[1], p[2], old[k], new[k], d, (d > noise || d < -noise) ? "!" : "≈"
        }
      }
    }
  ' "$1" "$2" | sort | awk -F'\t' -v h="$header" 'BEGIN { n = split(h, titles, ";"); printf "| %s | %s | %s | %s | %s |\n", titles[1], titles[2], titles[3], titles[4], titles[5]; print "| --- | --- | ---: | ---: | ---: |" } { printf "| %s | %s | %s | %s | %s%s |\n", $1, $2, $3, $4, $5, $6 }'
}

# 统计回退超过 $3% 的指标条数
count_regressed() { # $1 旧 $2 新 $3 限制%  →  条数
  awk -F'\t' -v lim="$3" '
    NR == FNR { old[$1 "|" $2] = $3; next }
    { k = $1 "|" $2; if (k in old && old[k] + 0 > 0) { d = ($3 - old[k]) / old[k] * 100; if (d > lim) n++ } }
    END { print n + 0 }
  ' "$1" "$2"
}

# 提取 sweep summary 的计时/资源字段为两列 TSV（键 <TAB> 毫秒或字节）
sweep_metrics_tsv() { # $1 = sweep-summary.json, $2 = 输出 tsv
  {
    jq -r '.phases // {} | to_entries[] | select(.value != null) | "\(.key)\t\(.value)"' "$1"
    jq -r '.server_phases // {} | to_entries[] | select(.value != null) | "server.\(.key)\t\(.value)"' "$1"
    jq -r '.resources.peak_working_set_bytes // empty | "peak_rss_bytes\t\(.)"' "$1"
  } > "$2"
}

latest_sweep_summary() { # $1 = sweep 输出目录
  if [ -f "$1/sweep-summary.json" ]; then echo "$1/sweep-summary.json"; return; fi
  ls -1t "$1"/sweep-*.json 2>/dev/null | grep -v '/sweep-summary.json$' | head -1 || true
}

# 用法: run_sweep <输出目录> <标签> [previous-summary.json|""]
run_sweep() {
  local out_dir="$1" label="$2" previous="${3:-}"
  local bin
  bin="$(build_server)"
  mkdir -p "$out_dir"

  local args=(
    "$REPO_ROOT/editors/vscode/scripts/sweep.mjs"
    --server "$bin"
    --vanilla-source "$PDC_VANILLA_SOURCE"
    --vanilla-cache "$PDC_CACHE_ROOT/$PDC_GAME_ID/vanilla.pdcindex"
    --output "$out_dir"
    --label "$label"
  )
  [ -n "$previous" ] && args+=(--previous "$previous")

  c_info "全量 Vanilla sweep（标签 %s，语料 %s）…" "$label" "$PDC_VANILLA_SOURCE"
  # tee 的展示流走 stderr，保持本函数 stdout 只有最后的 summary 路径
  (cd "$REPO_ROOT" && node "${args[@]}") 2>&1 | tee "$out_dir/sweep-stdout.log" >&2

  local summary
  summary="$(latest_sweep_summary "$out_dir")"
  [ -n "$summary" ] || die "sweep 未生成 sweep-summary.json（查看 $out_dir/sweep-stdout.log）"
  echo "$summary"
}

corpus_manifest_update() { # $1 = 名称, $2 = 来源, $3 = 目标目录
  mkdir -p "$DATA_DIR"
  local files bytes tmp="$DATA_DIR/.manifest.tmp.json"
  files="$(find "$3" -type f | wc -l)"
  bytes="$(du -sb "$3" | cut -f1)"
  [ -f "$DATA_DIR/manifest.json" ] || echo '{}' > "$DATA_DIR/manifest.json"
  jq --arg name "$1" --arg source "$2" --arg at "$(date -Is)" \
     --argjson files "$files" --argjson bytes "$bytes" \
    '.[$name] = {source: $source, copied_at: $at, files: $files, bytes: $bytes}' \
    "$DATA_DIR/manifest.json" > "$tmp"
  mv "$tmp" "$DATA_DIR/manifest.json"
  c_ok "%s: %d 文件 / %.1f MB → %s" "$1" "$files" "$(awk -v b="$bytes" 'BEGIN{print b/1048576}')" "$3"
}

# ----------------------------------------------------------------------------- 子命令

cmd_init() {
  local install="${1:-}" missing=0

  for tool in cargo node jq git sha256sum column rsync; do
    command -v "$tool" >/dev/null || { c_warn "缺少 $tool"; missing=1; }
  done
  if ! command -v hyperfine >/dev/null; then
    c_warn "缺少 hyperfine（对照组计时）"
    if [ "$install" = "--install" ]; then sudo apt-get install -y hyperfine && command -v hyperfine >/dev/null || missing=1
    else missing=1; fi
  fi
  if [ -z "$SAMPLY_BIN" ] && [ -z "$PERF_BIN" ]; then
    c_warn "缺少 samply 与 perf（画像能力）"
    if [ "$install" = "--install" ]; then
      sudo apt-get install -y linux-tools-generic || true
      PERF_BIN="$(ls -1 /usr/lib/linux-tools/*/perf 2>/dev/null | sort -V | tail -1 || true)"
      [ -z "$PERF_BIN" ] && missing=1
    else
      missing=1
    fi
    c_warn "建议（可选）：cargo install samply   # 交互式火焰图（Firefox Profiler）"
  fi
  if [ ! -d "$PDC_VANILLA_COPY" ]; then
    c_warn "本地语料副本缺失（./perf.sh import-corpus all 搬运；缺失时退回原安装路径，9p 偏慢）"
    missing=1
  fi
  if [ ! -x "$CWTOOLS_NATIVE_BIN" ]; then
    c_warn "native 对照组二进制缺失（./perf.sh control --build 从 CWTOOLS_REPO 构建）"
  fi

  c_info "仓库      %s" "$REPO_ROOT"
  c_info "语料      %s" "$PDC_VANILLA_SOURCE"
  c_info "EDG 语料  %s" "$EDG_DIR"
  c_info "缓存根    %s（冷跑需先手动移除其中的 .pdcindex）" "$PDC_CACHE_ROOT"
  c_info "对照组    %s" "$CWTOOLS_NATIVE_BIN"
  c_info "规则哈希  %s" "$(rule_hash)"
  [ "$missing" = 0 ] && c_ok "环境自检通过" || c_warn "存在缺失项（--install 可补装系统包；import-corpus / control --build 补语料与对照组）"
}

cmd_import_corpus() {
  local what="${1:-}" prune=""
  shift || true
  [ "${1:-}" = "--prune" ] && prune="--delete"
  { [ "$what" = all ] || [ "$what" = vanilla ] || [ "$what" = edg ]; } \
    || die "用法: ./perf.sh import-corpus vanilla|edg|all [--prune]"

  local includes=(--include='*/')
  local ext
  for ext in $CORPUS_EXTENSIONS; do includes+=(--include="*.$ext"); done
  includes+=(--exclude='*')

  if [ "$what" = all ] || [ "$what" = vanilla ]; then
    local src
    src="$(vanilla_origin)"
    [ -d "$src" ] || die "vanilla 原始安装不存在：$src（可用 PDC_VANILLA_ORIGIN 覆盖）"
    c_info "搬运 vanilla 文本数据：%s → %s（--prune=%s）" "$src" "$PDC_VANILLA_COPY" "${prune:+on}"
    mkdir -p "$PDC_VANILLA_COPY"
    rsync -a --info=stats1 --prune-empty-dirs $prune "${includes[@]}" "$src"/ "$PDC_VANILLA_COPY"/
    corpus_manifest_update vanilla "$src" "$PDC_VANILLA_COPY"
  fi
  if [ "$what" = all ] || [ "$what" = edg ]; then
    [ -d "$EDG_WORKSHOP_SOURCE" ] || die "EDG workshop 目录不存在：$EDG_WORKSHOP_SOURCE（可用 EDG_WORKSHOP_SOURCE 覆盖）"
    c_info "搬运 EDG 文本数据：%s → %s（--prune=%s）" "$EDG_WORKSHOP_SOURCE" "$EDG_DIR" "${prune:+on}"
    mkdir -p "$EDG_DIR"
    rsync -a --info=stats1 --prune-empty-dirs $prune "${includes[@]}" "$EDG_WORKSHOP_SOURCE"/ "$EDG_DIR"/
    corpus_manifest_update edg "$EDG_WORKSHOP_SOURCE" "$EDG_DIR"
  fi
  c_info "注意：语料变更会使既有基线的 sweep 数字不可比，重要对比前请勿更新语料。"
}

cmd_status() {
  echo "== lab/perf 状态 =="
  if [ -f "$DATA_DIR/manifest.json" ]; then
    echo "-- 语料（data/，本地） --"
    jq -r 'to_entries[] | "  \(.key)\t\(.value.files) 文件 / \(.value.bytes/1048576 | floor)MB\t\(.value.source)"' \
      "$DATA_DIR/manifest.json" | column -t -s$'\t'
  else
    c_warn "本地语料未搬运（./perf.sh import-corpus all）"
  fi
  echo
  echo "-- 对照组 --"
  if [ -x "$CWTOOLS_NATIVE_BIN" ]; then
    echo "  native   $("$CWTOOLS_NATIVE_BIN" --version)  sha256 $(sha256sum "$CWTOOLS_NATIVE_BIN" | cut -c1-16)"
  else
    echo "  native   缺（./perf.sh control --build）"
  fi
  echo
  if [ -L "$BASELINES_DIR/current" ]; then
    local name meta
    name="$(basename "$(readlink -f "$BASELINES_DIR/current")")"
    meta="$BASELINES_DIR/$name/metadata.json"
    c_ok "当前基线: %s" "$name"
    [ -f "$meta" ] && jq -r '"  commit \(.git.rev[0:10]) (\(.git.branch)\(if .git.dirty then ", dirty" else "" end)) @ \(.created_at)  语料 \(.corpus)"' "$meta" 2>/dev/null
    [ -f "$BASELINES_DIR/$name/sweep-summary.json" ] && \
      jq -r '"  sweep: files=\(.summary.files_analyzed) diags=\(.summary.total_diagnostics) total=\(.phases.total_ms | floor)ms"' \
        "$BASELINES_DIR/$name/sweep-summary.json"
  else
    c_warn "尚无基线（先运行: ./perf.sh baseline <name>）"
  fi
  echo
  echo "-- 最近运行 --"
  ls -1t "$RUNS_DIR" 2>/dev/null | head -5 || echo "  （无）"
}

cmd_bench() {
  local repeat="$BENCH_REPEAT"
  local extra=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --repeat) repeat="$2"; shift 2 ;;
      --) shift; extra=("$@"); break ;;
      *) extra+=("$1"); shift ;;
    esac
  done
  local run
  run="$(new_run_dir bench)"
  c_info "cargo bench（repeat=%d）%s" "$repeat" "${extra[*]:+过滤: ${extra[*]}}"
  run_bench_suite "$run" "$repeat" "${extra[@]}"
  column -t -s $'\t' "$run/bench.tsv"
}

cmd_baseline() {
  local name="" skip_bench=0 skip_sweep=0 force=0 repeat="$BENCH_REPEAT"
  while [ $# -gt 0 ]; do
    case "$1" in
      --skip-bench) skip_bench=1; shift ;;
      --skip-sweep) skip_sweep=1; shift ;;
      --force) force=1; shift ;;
      --repeat) repeat="$2"; shift 2 ;;
      *) name="$1"; shift ;;
    esac
  done
  [ -n "$name" ] || die "用法: ./perf.sh baseline <name> [--skip-bench|--skip-sweep|--repeat N|--force]"
  [[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || die "基线名只允许 [A-Za-z0-9._-] 且不能以 - 开头"
  case "$name" in -*) die "基线名不能以 - 开头";; esac
  local dir="$BASELINES_DIR/$name"
  if [ -e "$dir" ] && [ "$force" != 1 ]; then die "基线已存在：$dir（--force 覆盖）"; fi
  rm -rf "$dir"; mkdir -p "$dir/sweep"

  local bin
  bin="$(build_server)"
  cp "$bin" "$dir/paradoxcode"

  [ "$skip_bench" != 1 ] && run_bench_suite "$dir" "$repeat"

  if [ "$skip_sweep" != 1 ]; then
    local summary
    summary="$(run_sweep "$dir/sweep" "baseline:$name" "")"
    ln -sfn "sweep/sweep-summary.json" "$dir/sweep-summary.json"
    c_ok "基线 sweep: files=%s diags=%s total=%s ms" \
      "$(jq -r '.summary.files_analyzed' "$summary")" \
      "$(jq -r '.summary.total_diagnostics' "$summary")" \
      "$(jq -r '.phases.total_ms | floor' "$summary")"
  fi

  jq -n --arg name "$name" --arg created "$(date -Is)" \
        --arg bin_sha "$(sha256sum "$dir/paradoxcode" | cut -d' ' -f1)" \
        --arg rule_hash "$(rule_hash)" \
        --arg corpus "$PDC_VANILLA_SOURCE" --argjson bench_repeat "$repeat" \
        --argjson git "$(git_state_json)" \
    '{name: $name, created_at: $created, binary_sha256: $bin_sha, rule_hash: $rule_hash,
      corpus: $corpus, bench_repeat: $bench_repeat, git: $git}' \
    > "$dir/metadata.json"
  ln -sfn "$name" "$BASELINES_DIR/current"
  c_ok "基线 %s 完成并设为 current（语料 %s，bench_repeat=%d）" "$name" "$PDC_VANILLA_SOURCE" "$repeat"
}

cmd_sweep() {
  local label="manual" previous="" cold=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --label) label="$2"; shift 2 ;;
      --previous) previous="$2"; shift 2 ;;
      --cold) cold=1; shift ;;
      *) die "未知参数: $1" ;;
    esac
  done
  if [ "$cold" = 1 ]; then
    c_info "冷跑检查：缓存根 %s" "$PDC_CACHE_ROOT"
    if find "$PDC_CACHE_ROOT" -name '*.pdcindex' -print -quit 2>/dev/null | grep -q .; then
      die "检测到既有 .pdcindex。按仓库协议脚本不代删缓存，请先手动移除后再 --cold（或去掉 --cold 跑热态）。"
    fi
    c_ok "未发现 .pdcindex，按冷启动协议执行（服务器会在显式缓存缺失时原位重建）"
  fi
  local run summary
  run="$(new_run_dir sweep)"
  summary="$(run_sweep "$run/sweep" "$label${cold:+-cold}" "$previous")"
  sweep_metrics_tsv "$summary" "$run/metrics.tsv"
  column -t -s $'\t' "$run/metrics.tsv"
  c_ok "报告: %s" "$summary"
}

cmd_ab() {
  local base="" bench_only=0 sweep_only=0 fail_over="" repeat="$BENCH_REPEAT"
  while [ $# -gt 0 ]; do
    case "$1" in
      --bench-only) bench_only=1; shift ;;
      --sweep-only) sweep_only=1; shift ;;
      --fail-over) fail_over="$2"; shift 2 ;;
      --repeat) repeat="$2"; shift 2 ;;
      *) base="$1"; shift ;;
    esac
  done
  if [ -z "$base" ] && [ -L "$BASELINES_DIR/current" ]; then
    base="$(basename "$(readlink -f "$BASELINES_DIR/current")")"
  fi
  [ -n "$base" ] || die "未指定基线且无 baselines/current（先 ./perf.sh baseline <name>）"
  local bdir="$BASELINES_DIR/$base"
  [ -f "$bdir/metadata.json" ] || die "基线不存在: $bdir"

  local b_corpus
  b_corpus="$(jq -r '.corpus // "unknown"' "$bdir/metadata.json")"
  if [ "$b_corpus" != "$PDC_VANILLA_SOURCE" ]; then
    c_warn "基线语料（%s）与当前语料（%s）不同，sweep 对比不可比，仅报告基准部分。" "$b_corpus" "$PDC_VANILLA_SOURCE"
  fi

  local run
  run="$(new_run_dir "ab-$base")"
  local report="$run/report.md"
  {
    echo "# A/B: 当前工作区 vs 基线 \`$base\`"
    echo
    echo "- 基线: $(jq -r '.git.rev[0:10]' "$bdir/metadata.json") ($(jq -r '.git.branch' "$bdir/metadata.json")) @ $(jq -r '.created_at' "$bdir/metadata.json")，语料 $(jq -r '.corpus' "$bdir/metadata.json")，bench_repeat=$(jq -r '.bench_repeat // 1' "$bdir/metadata.json")"
    echo "- 当前: $(git -C "$REPO_ROOT" rev-parse --short HEAD) ($(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD), dirty: $(git -C "$REPO_ROOT" status --porcelain | head -1 | grep -q . && echo yes || echo no))，语料 $PDC_VANILLA_SOURCE，bench_repeat=$repeat"
    echo "- 噪声阈值: ±${NOISE_PCT}%（≈ 视为噪声，! 值得关注）"
    echo
  } > "$report"

  local regressions=0

  if [ "$sweep_only" != 1 ] && [ -f "$bdir/bench.tsv" ]; then
    run_bench_suite "$run" "$repeat"
    { echo "## 仓库基准（cargo bench）"; echo
      compare_tsv_md "$bdir/bench.tsv" "$run/bench.tsv" "$NOISE_PCT" "基准;指标;基线 ms;当前 ms;Δ%"; echo; } >> "$report"
    if [ -n "$fail_over" ]; then
      regressions=$((regressions + $(count_regressed "$bdir/bench.tsv" "$run/bench.tsv" "$fail_over")))
    fi
  fi

  if [ "$bench_only" != 1 ] && [ -f "$bdir/sweep-summary.json" ] && [ "$b_corpus" = "$PDC_VANILLA_SOURCE" ]; then
    local summary
    summary="$(run_sweep "$run/sweep" "ab-vs-$base" "$bdir/sweep-summary.json")"
    sweep_metrics_tsv "$summary" "$run/metrics.tsv"
    sweep_metrics_tsv "$bdir/sweep-summary.json" "$run/baseline-metrics.tsv"
    # 两列 TSV 借第一列空位复用三列对比器
    awk -F'\t' 'BEGIN{OFS="\t"} {print "sweep", $1, $2}' "$run/baseline-metrics.tsv" > "$run/.b3"
    awk -F'\t' 'BEGIN{OFS="\t"} {print "sweep", $1, $2}' "$run/metrics.tsv" > "$run/.n3"
    { echo "## 全量 sweep 相位"; echo
      compare_tsv_md "$run/.b3" "$run/.n3" "$NOISE_PCT" "类别;指标;基线;当前;Δ%"; echo; } >> "$report"
    if [ -n "$fail_over" ]; then
      regressions=$((regressions + $(count_regressed "$run/.b3" "$run/.n3" "$fail_over")))
    fi
    local old_d new_d old_f new_f
    old_d=$(jq -r '.summary.total_diagnostics' "$bdir/sweep-summary.json")
    new_d=$(jq -r '.summary.total_diagnostics' "$summary")
    old_f=$(jq -r '.summary.files_analyzed' "$bdir/sweep-summary.json")
    new_f=$(jq -r '.summary.files_analyzed' "$summary")
    if [ "$old_d" != "$new_d" ] || [ "$old_f" != "$new_f" ]; then
      { echo "> ⚠ 诊断面漂移：files $old_f→$new_f，diagnostics $old_d→$new_d。工作量不同，计时对比仅供参考。"; echo; } >> "$report"
      c_warn "诊断面漂移：files %s→%s, diagnostics %s→%s" "$old_f" "$new_f" "$old_d" "$new_d"
    fi
  fi

  cat "$report" >&2
  c_ok "A/B 报告: %s" "$report"
  if [ -n "$fail_over" ]; then
    [ "$regressions" -gt 0 ] && die "有 $regressions 项指标回退超过 ${fail_over}%（--fail-over 判定失败）"
    c_ok "--fail-over %s%% 判定通过（0 项回退）" "$fail_over"
  fi
}

cmd_control() {
  local runs="$CONTROL_RUNS" warmup="$CONTROL_WARMUP" build=0 timings_only=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --runs) runs="$2"; shift 2 ;;
      --warmup) warmup="$2"; shift 2 ;;
      --build) build=1; shift ;;
      --timings-only) timings_only=1; shift ;;
      *) die "未知参数: $1" ;;
    esac
  done

  if [ "$build" = 1 ] || [ ! -x "$CWTOOLS_NATIVE_BIN" ]; then
    build_control_native
  fi

  local exe version
  exe="$CWTOOLS_NATIVE_BIN"
  version="$("$exe" --version) $(sha256sum "$exe" | cut -c1-16)"
  local run
  run="$(new_run_dir control-native)"
  local args=(validate --game "$CWTOOLS_GAME" -q --report-type json
    --rules "${CWTOOLS_RULES:?需在 config.local.sh 配置 CWTOOLS_RULES}"
    --directory "$PDC_VANILLA_SOURCE"
    --output-file "$run/report.json")
  c_info "对照组[native]: %s，语料 = %s（与实验组同一棵树）" "$version" "$PDC_VANILLA_SOURCE"

  c_info "采样相位计时（CWTOOLS_TIMINGS=1，[t] 行走 stderr）…"
  # cwtools 以 linter 语义退出：0=干净，1=存在 Error 级诊断（属正常）；>1 才是运行失败
  local exit_code=0
  CWTOOLS_TIMINGS=1 "$exe" "${args[@]}" > /dev/null 2> "$run/timings-stderr.log" || exit_code=$?
  if [ "$exit_code" -gt 1 ]; then
    c_warn "对照组运行退出码 %s（>1 才是失败），详见 %s" "$exit_code" "$run/timings-stderr.log"
  fi
  if grep -qE '^[[:space:]]*\[t\]' "$run/timings-stderr.log"; then
    grep -E '^[[:space:]]*\[t\]' "$run/timings-stderr.log" | tee "$run/timings.txt"
  else
    c_warn "未见 [t] 相位行（检查 %s）" "$run/timings-stderr.log"
    : > "$run/timings.txt"
  fi

  if [ "$timings_only" != 1 ]; then
    c_info "hyperfine 计时：预热 %d 次 + 计时 %d 次…（-i：对照组退出码 1=有诊断，属正常）" "$warmup" "$runs"
    # hyperfine 的每个位置参数是一条独立命令：整条命令经 %q 转义拼成单个字符串，
    # 交给它的默认 sh 执行（-N 模式无法带参数，弃用）
    local cmd
    cmd="$(printf '%q ' "$exe" "${args[@]}")"
    hyperfine -i --warmup "$warmup" --runs "$runs" \
      --export-json "$run/hyperfine.json" -- "$cmd"
    jq -r '.results[0] | "wall: min=\(.min)s mean=\(.mean)s max=\(.max)s stddev=\(.stddev)s"' "$run/hyperfine.json"
  fi

  # 历史行：时间 模式 版本 min mean 相位…
  [ -f "$RUNS_DIR/control-history.tsv" ] || printf '时间\t模式\t版本\tmin\tmean\t相位\n' > "$RUNS_DIR/control-history.tsv"
  {
    printf '%s\t%s\t%s\t' "$(date -Is)" native "$version"
    if [ -f "$run/hyperfine.json" ]; then
      jq -r '"\(.results[0].min)\t\(.results[0].mean)\t"' "$run/hyperfine.json"
    else
      printf '\t-\t-\t'
    fi
    tr '\n' ' ' < "$run/timings.txt"; echo
  } >> "$RUNS_DIR/control-history.tsv"
  c_ok "对照记录: %s（历史见 runs/control-history.tsv）" "$run"
}

cmd_profile() {
  [ $# -ge 1 ] || die "用法: ./perf.sh profile <bench:<名称>|sweep> [--flavor samply|perf]"
  local target="$1"; shift || true
  local flavor="auto"
  while [ $# -gt 0 ]; do
    case "$1" in
      --flavor) flavor="$2"; shift 2 ;;
      *) die "未知参数: $1" ;;
    esac
  done
  if [ "$flavor" = auto ]; then
    if [ -n "$SAMPLY_BIN" ]; then flavor=samply
    elif [ -n "$PERF_BIN" ]; then flavor=perf
    else die "无可用画像工具。安装任一：cargo install samply ／ sudo apt-get install linux-tools-generic"; fi
  fi

  local run
  run="$(new_run_dir "profile-${target//[:\/]/-}")"

  case "$target" in
    bench:*)
      local bench="${target#bench:}"
      local bench_rs
      bench_rs="$(ls "$REPO_ROOT"/crates/*/benches/"$bench".rs 2>/dev/null | head -1 || true)"
      [ -n "$bench_rs" ] || die "找不到基准 crates/*/benches/$bench.rs"
      local pkg
      pkg="$(basename "$(dirname "$(dirname "$bench_rs")")")"
      c_info "画像 cargo bench -p %s --bench %s（%s）" "$pkg" "$bench" "$flavor"
      if [ "$flavor" = samply ]; then
        (cd "$REPO_ROOT" && "$SAMPLY_BIN" record --save-to "$run/profile.json.gz" -- \
          cargo bench --locked -p "$pkg" --bench "$bench")
        c_ok "samply 数据: %s" "$run/profile.json.gz"
      else
        (cd "$REPO_ROOT" && "$PERF_BIN" record -F 199 -g -o "$run/perf.data" -- \
          cargo bench --locked -p "$pkg" --bench "$bench")
        "$PERF_BIN" report -i "$run/perf.data" --stdio --sort symbol,dso | head -80 > "$run/hotspots.txt" || true
        c_ok "perf 数据: %s（热点表 %s）" "$run/perf.data" "$run/hotspots.txt"
      fi
      ;;
    sweep)
      local real
      real="$(build_server)"
      # shim：sweep 拉起的 server 落在采样器之下；规则哈希校验走 LSP 报文，不受 shim 影响
      mkdir -p "$run/shim"
      local shim="$run/shim/paradoxcode"
      if [ "$flavor" = samply ]; then
        printf '#!/usr/bin/env bash\nexec %q record --save-to %q -- %q "$@"\n' \
          "$SAMPLY_BIN" "$run/profile.json.gz" "$real" > "$shim"
      else
        printf '#!/usr/bin/env bash\nexec %q record -F 199 -g -o %q -- %q "$@"\n' \
          "$PERF_BIN" "$run/perf.data" "$real" > "$shim"
      fi
      chmod +x "$shim"
      c_info "画像 sweep（server 经 shim 包裹，产物在 %s）" "$run"
      (cd "$REPO_ROOT" && node editors/vscode/scripts/sweep.mjs \
        --server "$shim" --vanilla-source "$PDC_VANILLA_SOURCE" \
        --output "$run/sweep" --label profile) 2>&1 | tee "$run/sweep-stdout.log" >&2
      if [ "$flavor" = perf ] && [ -f "$run/perf.data" ]; then
        "$PERF_BIN" report -i "$run/perf.data" --stdio --sort symbol,dso | head -80 > "$run/hotspots.txt" || true
        c_ok "perf 热点表: %s" "$run/hotspots.txt"
      fi
      ;;
    *)
      die "未知画像目标: $target（支持 bench:<名称> 或 sweep）"
      ;;
  esac
}

cmd_help() { grep '^#   \./perf.sh' "$0" | sed 's/^#   //'; }

# ----------------------------------------------------------------------------- 入口
# 允许 `PERF_LIB_ONLY=1 source ./perf.sh` 只加载函数（供测试/复用），不执行子命令分发

if [ "${PERF_LIB_ONLY:-0}" != 1 ]; then
  case "${1:-help}" in
    init) shift; cmd_init "$@" ;;
    import-corpus) shift; cmd_import_corpus "$@" ;;
    status) cmd_status ;;
    bench) shift; cmd_bench "$@" ;;
    baseline) shift; cmd_baseline "$@" ;;
    sweep) shift; cmd_sweep "$@" ;;
    ab) shift; cmd_ab "$@" ;;
    control) shift; cmd_control "$@" ;;
    profile) shift; cmd_profile "$@" ;;
    help|--help|-h) cmd_help ;;
    *) die "未知子命令: $1（./perf.sh help 查看用法）" ;;
  esac
fi
