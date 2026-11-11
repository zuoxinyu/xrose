"""Smoke-test the actual install images on their native OS without starting a GUI."""
import argparse
import contextlib
import json
import os
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from release import LOCK, digest, run


@contextlib.contextmanager
def extract(package, root):
    root.mkdir()
    if package.suffix == ".dmg":
        run("hdiutil", "verify", package, stdout=subprocess.DEVNULL)
        run("hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", root, package)
        try:
            app = root / "Xrose.app"
            run("codesign", "--verify", "--strict", app)
            yield app / "Contents/MacOS/xrose-app", app / "Contents/Resources/xrose-seed"
        finally:
            run("hdiutil", "detach", root)
        return
    if package.suffix == ".zip":
        with zipfile.ZipFile(package) as archive:
            archive.extractall(root)
        yield root / "Xrose.exe", root / "resources/xrose-seed"
        return
    if package.suffix == ".deb":
        run("dpkg-deb", "--extract", package, root)
    elif package.suffix == ".zst":
        run("tar", "--zstd", "-xf", package, "-C", root)
    else:
        run(package, "--appimage-extract", cwd=root, stdout=subprocess.DEVNULL)
        root = root / "squashfs-root"
    yield (root / "AppRun" if package.suffix == ".AppImage" else root / "usr/bin/xrose-app",
           root / "usr/share/xrose/xrose-seed")


def prepare(app, profile):
    run(app, "--prepare-resources", "--home", profile, env=dict(os.environ, XROSE_HOME=str(profile)))


def verify(full, lite, tmp):
    profile = tmp / "profile"
    suffix = ".exe" if os.name == "nt" else ""
    data = profile / "data"
    cli = data / "bin" / ("xrose-cli" + suffix)
    core = data / "cores/sing-box" / LOCK["version"] / ("sing-box" + suffix)
    with extract(full, tmp / "full") as (app, seed):
        if not seed.is_dir():
            raise ValueError("full 包缺少随附资源")
        prepare(app, profile)
        if digest(cli) != digest(seed / cli.name):
            raise ValueError("首次安装的 CLI 不匹配")
        env = dict(os.environ, XROSE_HOME=str(profile))
        mesh = run(cli, "mesh", "list", "--home", profile, stdout=subprocess.PIPE, env=env)
        if json.loads(mesh.stdout) != []:
            raise ValueError("发布包包含非空组网数据")
        run(cli, "check", "--core", "sing-box", "--home", profile, env=env)
        for name, rule in LOCK["rules"].items():
            file = data / "rulesets" / name
            if digest(file) != rule["sha256"]:
                raise ValueError(f"首次安装的规则集不匹配：{name}")
            # Decode each binary with the shipped core; no network or TUN needed.
            run(core, "rule-set", "decompile", file, "-o", tmp / "rule.json", env=env)
        rule_file = data / "rulesets/geoip-cn.srs"
        rule_file.write_bytes(b"local update")
        installed = {p: digest(p) for p in (cli, core, rule_file)}
        prepare(app, profile)
    with extract(lite, tmp / "lite") as (app, seed):
        if seed.exists():
            raise ValueError("lite 包中存在随附资源")
        empty = tmp / "lite-profile"
        prepare(app, empty)
        if (empty / "data/cores").exists():
            raise ValueError("lite 在空目录中安装了内核")
        prepare(app, profile)
    if any(digest(p) != value for p, value in installed.items()):
        raise ValueError("再次启动或 lite 更新覆盖了已有文件")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    out = parser.parse_args().directory.resolve()
    packages = sorted(out.glob("Xrose-*-full.*"))
    expected = 3 if sys.platform == "linux" and os.uname().machine == "x86_64" else 2 if sys.platform == "linux" else 1
    if len(packages) != expected:
        raise ValueError("缺少当前平台的发布包或目录含多个版本，请使用独立的发布目录")
    for full in packages:
        with tempfile.TemporaryDirectory() as tmp:
            verify(full, full.with_name(full.name.replace("-full.", "-lite.")), Path(tmp))
        print(f"full/lite 首次运行与更新验证通过：{full.name}", flush=True)


if __name__ == "__main__":
    main()
