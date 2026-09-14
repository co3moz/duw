#!/usr/bin/env bash
# Regenerates docs/demo.gif, the animation embedded in the README.
#
# It runs the hidden `--demo` mode, records the streaming scan with headless
# Chrome, and encodes the frames with ffmpeg. Nothing is read from disk and no
# demo tree needs to be prepared.
#
# Usage: docs/generate_screenshot.sh [--out <gif>] [--port <n>] [--debug-port <n>]
#                                    [--keep-frames]
#
# Needs cargo, node 22.4+ and Chrome/Chromium. ffmpeg is picked up from PATH,
# or installed once with npm when missing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v node >/dev/null 2>&1; then
  echo "generate_screenshot.sh: node 22.4+ is required" >&2
  exit 1
fi

exec node "$ROOT/docs/screencast.mjs" "$@"
