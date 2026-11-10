"""Implementation of build-macos.sh. Requires macOS and the Xcode command line tools."""

import argparse
import ctypes
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
BUNDLE_ID = "io.xrose.Xrose"


def run(*args, **kwargs):
    return subprocess.run(args, cwd=ROOT, check=True, **kwargs)


def signing_identity():
    override = os.getenv("XROSE_SIGN_IDENTITY")
    if override:
        return override

    # Request only valid identities with a private key. Use a fingerprint so
    # duplicate display names or keychain ordering cannot change the selection.
    listing = run("/usr/bin/security", "find-identity", "-v", "-p", "codesigning",
                  stdout=subprocess.PIPE).stdout
    matches = re.findall(rb'^\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"Apple Development:[^"\n]+"',
                         listing, re.MULTILINE)
    if matches:
        print("使用本机 Apple Development 证书签名", flush=True)
        return min(matches).decode("ascii")
    print("未找到有效的 Apple Development 证书，使用临时签名（ad hoc）", flush=True)
    return "-"


def build(profile):
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps",
                              "--format-version=1", stdout=subprocess.PIPE).stdout)
    package = next(p for p in metadata["packages"] if p["name"] == "xrose-app")
    command = ["cargo", "build", "--locked", "-p", "xrose-app", "--bin", "xrose-app",
               "--message-format=json-render-diagnostics"]
    if profile == "release":
        command.append("--release")
    # Cargo reports the actual executable, including a configured target triple.
    messages = run(*command, stdout=subprocess.PIPE).stdout.splitlines()
    for msg in map(json.loads, messages):
        if (msg["reason"] == "compiler-artifact" and msg["package_id"] == package["id"]
                and msg.get("executable") and msg["target"]["name"] == "xrose-app"):
            return (Path(msg["executable"]), package["version"],
                    Path(metadata["target_directory"]) / "macos" / profile / "Xrose.app")
    raise ValueError("Cargo 未返回 xrose-app 的构建路径")


def assemble(app, binary, version, identity):
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources").mkdir()
    # Bare development binaries can have a Finder custom icon. Do not inherit
    # resource forks or other extended attributes when preparing the signed copy.
    executable = contents / "MacOS/xrose-app"
    shutil.copyfile(binary, executable)
    executable.chmod(0o755)

    with tempfile.TemporaryDirectory() as tmp:
        icons = Path(tmp) / "Xrose.iconset"
        icons.mkdir()
        source = ROOT / "crates/xrose-app/assets/app-icon.png"
        for size in (16, 32, 128, 256, 512):
            for scale, suffix in ((1, ""), (2, "@2x")):
                pixels = str(size * scale)
                run("sips", "-z", pixels, pixels, source, "--out",
                    icons / f"icon_{size}x{size}{suffix}.png", stdout=subprocess.DEVNULL)
        icon = Path(tmp) / "Xrose.icns"
        run("iconutil", "-c", "icns", icons, "-o", icon)
        shutil.copyfile(icon, contents / "Resources/Xrose.icns")

    with (contents / "Info.plist").open("wb") as plist:
        plistlib.dump(dict(CFBundleInfoDictionaryVersion="6.0", CFBundleIdentifier=BUNDLE_ID,
                           CFBundleName="Xrose", CFBundleDisplayName="Xrose",
                           CFBundleExecutable="xrose-app", CFBundlePackageType="APPL",
                           CFBundleVersion=version, CFBundleShortVersionString=version,
                           CFBundleIconFile="Xrose.icns", NSHighResolutionCapable=True), plist)
    run("codesign", "--force", "--sign", identity, "--identifier", BUNDLE_ID, app)


def publish(prepared, dest):
    run("codesign", "--verify", "--strict", prepared)
    if dest.is_symlink():
        raise ValueError(f"拒绝替换符号链接：{dest}")
    exists = dest.exists()
    if exists:
        with (dest / "Contents/Info.plist").open("rb") as plist:
            if plistlib.load(plist).get("CFBundleIdentifier") != BUNDLE_ID:
                raise ValueError(f"目标不是 Xrose 应用包：{dest}")
        if subprocess.run(["/usr/sbin/lsof", "-t", dest / "Contents/MacOS/xrose-app"],
                          stdout=subprocess.DEVNULL).returncode == 0:
            raise ValueError(f"请先退出正在使用此应用包的 Xrose，再重新运行脚本：{dest}")

    # Both paths are on the same volume. A failed swap leaves the old bundle
    # in place; a successful swap leaves it in our private temporary directory.
    rename = ctypes.CDLL(None, use_errno=True).renamex_np
    rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    flag = 0x2 if exists else 0x4  # Darwin RENAME_SWAP / RENAME_EXCL
    if rename(os.fsencode(prepared), os.fsencode(dest), flag) != 0:
        raise OSError(ctypes.get_errno(), "无法替换应用包", str(dest))


def main():
    parser = argparse.ArgumentParser(
        prog="scripts/build-macos.sh", description="构建并签名 Xrose.app",
        epilog="构建目录遵循 Cargo 配置和 CARGO_TARGET_DIR。"
               "默认使用本机有效的 Apple Development 证书，找不到时使用临时签名。"
               "可用 XROSE_SIGN_IDENTITY 指定签名身份，设为 - 可强制使用临时签名。")
    profiles = parser.add_mutually_exclusive_group()
    profiles.add_argument("-r", "--release", dest="profile", action="store_const", const="release", help="构建 release 版本")
    profiles.add_argument("-d", "--debug", dest="profile", action="store_const", const="debug", help="构建 debug 版本（默认）")
    parser.set_defaults(profile="debug")
    parser.add_argument("--install-dir", type=Path, default=os.getenv("XROSE_APP_DIR") or Path.home() / "Applications",
                        help="安装目录，默认取 XROSE_APP_DIR 或 ~/Applications")
    parser.add_argument("--no-install", action="store_true", help="仅生成应用包")
    parser.add_argument("--open", action="store_true", help="打开安装后的应用包；仅生成时打开构建产物")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("此脚本仅用于 macOS")

    binary, version, app = build(args.profile)
    dest = app if args.no_install else args.install_dir.resolve() / app.name
    if not args.no_install and dest.parent in (Path("/"), Path.home().resolve()):
        raise ValueError("请指定应用程序目录，不能直接安装到根目录或用户主目录")
    if dest in app.parents or app in dest.parents:
        raise ValueError("构建与安装的应用包不能相互嵌套")
    app.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=app.parent) as tmp:
        prepared = Path(tmp) / app.name
        assemble(prepared, binary, version, signing_identity())
        publish(prepared, app)
    print(f"已生成：{app}", flush=True)
    if dest != app:
        dest.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=dest.parent) as tmp:
            prepared = Path(tmp) / dest.name
            run("ditto", app, prepared)
            publish(prepared, dest)
        print(f"已安装：{dest}", flush=True)
    if args.open:
        command = ["open", "-n", "-a", dest]
        if os.getenv("XROSE_HOME"):
            command.extend(["--args", "--home", Path(os.environ["XROSE_HOME"]).resolve()])
        run(*command)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, plistlib.InvalidFileException, subprocess.CalledProcessError) as error:
        sys.exit(f"打包失败：{error}")
