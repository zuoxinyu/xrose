#!/usr/bin/env python3
"""Cross-platform helper (Windows/macOS/Linux) to crop a full-desktop PNG around
an Xrose window with outer wallpaper padding and encode to WebP.

Requires: ffmpeg & ffprobe in PATH (Python standard library only).
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


def run_cua_call(tool: str, payload: dict) -> dict | str:
    cua_driver = shutil.which("cua-driver")
    if not cua_driver:
        raise RuntimeError("cua-driver not found in PATH; pass --rect x,y,w,h instead.")
    proc = subprocess.run(
        [cua_driver, "call", tool, json.dumps(payload)],
        capture_output=True,
        text=True,
        check=True,
    )
    out = proc.stdout.strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return out


def query_window_bounds(pid: int, window_id: int | None) -> tuple[float, float, float, float]:
    data = run_cua_call("list_windows", {"pid": pid})
    if not isinstance(data, dict):
        raise RuntimeError(f"Unexpected list_windows output: {data}")
    windows = data.get("windows") or []
    if not windows:
        raise RuntimeError(f"No windows returned by cua-driver for pid={pid}.")

    target = None
    if window_id is not None:
        for win in windows:
            if win.get("window_id") == window_id:
                target = win
                break
        if target is None:
            raise RuntimeError(f"window_id={window_id} not found for pid={pid}.")
    else:
        on_screen = [w for w in windows if w.get("is_on_screen") and not w.get("minimized")]
        target = on_screen[0] if on_screen else windows[0]

    b = target["bounds"]
    return float(b["x"]), float(b["y"]), float(b["width"]), float(b["height"])


def query_display_scale() -> float:
    """Best-effort query of display scale factor from cua-driver get_screen_size."""
    try:
        data = run_cua_call("get_screen_size", {})
    except Exception:
        return 1.0

    if isinstance(data, dict):
        for key in ("scale_factor", "scale", "backing_scale_factor"):
            val = data.get(key)
            if isinstance(val, (int, float)) and val > 0:
                return float(val)
    elif isinstance(data, str):
        m = re.search(r"@\s*([0-9]+(?:\.[0-9]+)?)x", data)
        if m:
            return float(m.group(1))
    return 1.0


def resolve_bounds_scale(scale_arg: str, display_scale: float) -> float:
    """Resolve multiplier that maps list_windows bounds -> screenshot physical pixels."""
    if scale_arg != "auto":
        val = float(scale_arg)
        if val <= 0:
            raise ValueError("--scale must be > 0")
        return val

    # On Windows, cua-driver list_windows bounds and get_desktop_state are both physical px.
    if sys.platform == "win32":
        return 1.0
    # On macOS, list_windows bounds are logical points while native screenshots are physical px.
    if sys.platform == "darwin":
        return display_scale
    # On Linux, bounds are typically physical px; callers can pass --scale explicitly if scaled.
    return 1.0


def probe_image_size(path: Path) -> tuple[int, int]:
    ffprobe = shutil.which("ffprobe")
    if not ffprobe:
        raise RuntimeError("ffprobe not found in PATH.")
    proc = subprocess.run(
        [
            ffprobe,
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
            str(path),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    w_str, h_str = proc.stdout.strip().split("x")
    return int(w_str), int(h_str)


def parse_rect(spec: str) -> tuple[float, float, float, float]:
    parts = [float(p.strip()) for p in spec.split(",")]
    if len(parts) != 4:
        raise argparse.ArgumentTypeError("--rect must be x,y,w,h (4 numbers)")
    return parts[0], parts[1], parts[2], parts[3]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Crop a full-desktop screenshot around Xrose and encode to WebP."
    )
    parser.add_argument("--input", required=True, type=Path, help="Input full-desktop PNG")
    parser.add_argument("--output", required=True, type=Path, help="Output .webp path")
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--pid", type=int, help="Xrose PID to query live bounds via cua-driver")
    group.add_argument("--rect", type=parse_rect, help="Explicit window bounds: x,y,w,h")
    parser.add_argument("--window-id", type=int, default=None, help="Optional HWND / window_id")
    parser.add_argument(
        "--scale",
        default="auto",
        help="Multiplier from window bounds to screenshot px: 'auto' (default) or a float (e.g. 1, 2)",
    )
    parser.add_argument("--padding", type=float, default=30.0, help="Outer margin size (default: 30)")
    parser.add_argument(
        "--padding-unit",
        choices=("px", "dp"),
        default="px",
        help="Unit for --padding: physical 'px' (default) or logical 'dp' (scaled by display scale)",
    )
    parser.add_argument("--quality", type=int, default=90, help="WebP quality 0-100 (default: 90)")
    parser.add_argument("--lossless", action="store_true", help="Use lossless WebP encoding")
    args = parser.parse_args()

    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        print("Error: ffmpeg not found in PATH.", file=sys.stderr)
        return 1

    if not args.input.is_file():
        print(f"Error: input file not found: {args.input}", file=sys.stderr)
        return 1

    img_w, img_h = probe_image_size(args.input)
    display_scale = query_display_scale()
    bounds_scale = resolve_bounds_scale(args.scale, display_scale)

    if args.rect is not None:
        rx, ry, rw, rh = args.rect
    else:
        rx, ry, rw, rh = query_window_bounds(args.pid, args.window_id)

    wx = round(rx * bounds_scale)
    wy = round(ry * bounds_scale)
    ww = round(rw * bounds_scale)
    wh = round(rh * bounds_scale)

    pad_px = round(max(0.0, args.padding) * (display_scale if args.padding_unit == "dp" else 1.0))

    cx = max(0, wx - pad_px)
    cy = max(0, wy - pad_px)
    cw = min(img_w - cx, (wx + ww + pad_px) - cx)
    ch = min(img_h - cy, (wy + wh + pad_px) - cy)

    if cw <= 0 or ch <= 0:
        print(
            f"Error: computed empty crop rect ({cx},{cy},{cw},{ch}) for image {img_w}x{img_h}",
            file=sys.stderr,
        )
        return 1

    args.output.parent.mkdir(parents=True, exist_ok=True)

    cmd = [
        ffmpeg,
        "-y",
        "-v",
        "error",
        "-i",
        str(args.input),
        "-vf",
        f"crop={cw}:{ch}:{cx}:{cy}",
        "-c:v",
        "libwebp",
        "-compression_level",
        "6",
    ]
    if args.lossless:
        cmd.extend(["-lossless", "1"])
    else:
        cmd.extend(["-quality", str(args.quality)])
    cmd.append(str(args.output))

    subprocess.run(cmd, check=True)

    out_w, out_h = probe_image_size(args.output)
    size_kb = args.output.stat().st_size / 1024.0
    print(
        json.dumps(
            {
                "platform": sys.platform,
                "output": str(args.output),
                "display_scale": display_scale,
                "bounds_scale": bounds_scale,
                "window_bounds_px": {"x": wx, "y": wy, "width": ww, "height": wh},
                "padding_px": pad_px,
                "crop_rect": {"x": cx, "y": cy, "width": cw, "height": ch},
                "output_size": {"width": out_w, "height": out_h},
                "file_size_kb": round(size_kb, 1),
            },
            ensure_ascii=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
