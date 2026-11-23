#!/bin/zsh
# Builds the widget with sample data (see live-markets/src/preview.rs), shows it in the SDK's
# desktop testbed and saves that window, and only that window, as a PNG.
#
#   tools/shot.sh <mode> <out.png>      mode: live (the real markets), or sample data: pre, post,
#                                       closed, offline, chart:1D, chart:5D, chart:1D:post, ...
#
# Needs the testbed built once:
#   cargo build --manifest-path bmc-sdk/Cargo.toml -p bmc-wasm-runtime --features testbed --bin testbed
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
root=${0:A:h:h}
mode=${1:?usage: tools/shot.sh <mode> <out.png>}
out=${2:?usage: tools/shot.sh <mode> <out.png>}
cd "$root"
pkill -x testbed 2>/dev/null || true
if [[ $mode == live ]]; then
  cargo build -q -p live-markets --target wasm32-unknown-unknown
else
  LM_PREVIEW=$mode cargo build -q -p live-markets --features preview --target wasm32-unknown-unknown
fi
bmc-sdk/target/debug/testbed --platform bmc100 --manifest live-markets/manifest.json \
  target/wasm32-unknown-unknown/debug/live_markets.wasm >/dev/null 2>&1 &
sleep 7
id=$(swift tools/window-id.swift testbed)
[[ -n $id ]] || { print -u2 "The testbed has no window"; pkill -x testbed || true; exit 1; }
screencapture -x -o -l "$id" "$out"
pkill -x testbed || true
