#!/bin/sh
# Build, sign and optionally install the macOS GUI using the Xcode command line tools.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$(uname -s)" != Darwin ]; then
    echo "此脚本仅用于 macOS" >&2
    exit 1
fi
# The /usr/bin/python3 shim injects SDKROOT and header paths into Cargo's
# environment, invalidating native build caches used by ordinary cargo builds.
python=$(/usr/bin/xcrun --find python3)
exec "$python" "$root/scripts/package_macos.py" "$@"
