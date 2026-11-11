#!/bin/sh
set -eu
[ "$(uname -s)" = Linux ] || { echo "请在 Linux 上运行此脚本" >&2; exit 1; }
exec python3 "$(dirname "$0")/release.py" "$@"
