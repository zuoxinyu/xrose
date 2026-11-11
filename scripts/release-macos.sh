#!/bin/sh
set -eu
[ "$(uname -s)" = Darwin ] || { echo "请在 macOS 上运行此脚本" >&2; exit 1; }
exec "$(xcrun --find python3)" "$(dirname "$0")/release.py" "$@"
