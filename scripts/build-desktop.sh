#!/usr/bin/env zsh
# 构建 clipsync-desktop release 二进制。
# macOS：本机直接构建。Windows：在 Parallels VM 内安装 Rust MSVC 工具链后
# 执行同一命令（本脚本仅 macOS 侧）。产物与缓存按 AGENTS.md 走外部卷
# （CARGO_TARGET_DIR 由 scripts/external-dev-env.zsh 配置）。
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/external-dev-env.zsh
[[ "$EXTERNAL_DEV_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]]

cargo build --release -p clipsync-desktop
echo "==> built: ${CARGO_TARGET_DIR:-target}/release/clipsync-desktop"
