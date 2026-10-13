#!/usr/bin/env bash
# Raptor 一键验证门禁（CI / 本地均可执行）
#
# 顺序执行 5 项检查，任何一步失败立即退出（非零码即门禁失败）：
#   1. 格式检查          2. clippy（含所有 target）
#   3. 全工作区测试（含 C ABI / overlay / header-sync 集成测试）
#   4. smoke_test（FFI 生命周期冒烟）
#   5. e2e_playback（真实播放回归：播放/暂停/seek/停止/退出 22 项断言）
#
# 用法: bash scripts/run_gates.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# 本机 Windows cargo 增量编译缓存易卡死，默认关闭增量
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

echo "==> [1/5] cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> [2/5] cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> [3/5] cargo test --workspace"
cargo test --workspace

echo "==> [4/5] smoke_test (FFI lifecycle)"
cargo run --release --example smoke_test -p raptor-ffi

echo "==> [5/5] e2e_playback (real playback regression)"
if [[ ! -f test_video.mp4 ]]; then
  echo "ERROR: test_video.mp4 缺失，无法执行真实播放回归" >&2
  exit 1
fi
cargo run --release --example e2e_playback -p raptor-ffi -- test_video.mp4

echo "==> ALL GATES PASSED"
