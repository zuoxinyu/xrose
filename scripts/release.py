"""Build native full and lite release packages. See docs/RELEASE.md."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from package_macos import ROOT, assemble, signing_identity

LOCK = json.loads((ROOT / "packaging/resources.lock.json").read_text())
ASSETS = ROOT / "crates/xrose-app/assets"
CACHE = ROOT / "target/release-cache"


def run(*args, **options):
    return subprocess.run(args, check=True, **options)


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def fetch(url, sha, name):
    directory = CACHE / sha
    directory.mkdir(parents=True, exist_ok=True)
    cached = directory / name
    if not cached.is_file() or digest(cached) != sha:
        with tempfile.TemporaryDirectory(dir=directory) as tmp:
            path = Path(tmp) / name
            run("curl", "--fail", "--location", "--retry", "3", "--silent", "--show-error",
                "--proto", "=https", "--proto-redir", "=https", url, "-o", path)
            if digest(path) != sha:
                raise ValueError(f"{name} 的 SHA-256 与发布清单不一致")
            path.replace(cached)
    return cached


def binaries():
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version=1",
                              cwd=ROOT, stdout=subprocess.PIPE).stdout)
    package = next(p for p in metadata["packages"] if p["name"] == "xrose-app")
    version = package["version"]
    if os.environ.get("GITHUB_REF_TYPE") == "tag" and os.environ["GITHUB_REF_NAME"] != "v" + version:
        raise ValueError("发布标签与 Cargo 工作区版本不一致")
    messages = run("cargo", "build", "--locked", "--release", "-p", "xrose-app", "-p", "xrose-cli",
                   "--bins", "--message-format=json-render-diagnostics", cwd=ROOT, stdout=subprocess.PIPE)
    paths = {}
    for line in messages.stdout.splitlines():
        msg = json.loads(line)
        if msg["reason"] == "compiler-artifact" and msg.get("executable"):
            paths[msg["target"]["name"]] = Path(msg["executable"])
    return version, paths["xrose-app"], paths["xrose-cli"]


def seed_files(dest, os_name, arch, cli):
    dest.mkdir()
    system = "darwin" if os_name == "macos" else os_name
    ext = "zip" if system == "windows" else "tar.gz"
    version = LOCK["version"]
    name = f"sing-box-{version}-{system}-{arch}.{ext}"
    url = f"https://github.com/SagerNet/sing-box/releases/download/v{version}/{name}"
    shutil.copyfile(fetch(url, LOCK["archives"][name], name), dest / name)
    for name, rule in LOCK["rules"].items():
        shutil.copyfile(fetch(rule["url"], rule["sha256"], name), dest / name)
    shutil.copy2(cli, dest / cli.name)
    shutil.copyfile(ROOT / "packaging/NOTICE", dest / "NOTICE")
    shutil.copyfile(ROOT / "packaging/resources.lock.json", dest / "resources.lock.json")


def darwin(root, output, app, version, seed):
    bundle = root / "Xrose.app"
    identity = signing_identity()
    distribution = bool(os.getenv("XROSE_MAC_DISTRIBUTION"))
    assemble(bundle, app, version, identity, seed, distribution=distribution)
    shutil.copyfile(ROOT / "LICENSE", root / "LICENSE.txt")
    run("codesign", "--verify", "--strict", bundle)
    (root / "Applications").symlink_to("/Applications")
    run("hdiutil", "create", "-volname", "Xrose", "-srcfolder", root, "-format", "UDZO", output)
    if distribution:
        run("codesign", "--sign", identity, "--timestamp", output)
    profile = os.getenv("XROSE_NOTARY_PROFILE")
    if profile:
        run("xcrun", "notarytool", "submit", output, "--keychain-profile", profile, "--wait")
        run("xcrun", "stapler", "staple", output)
        run("xcrun", "stapler", "validate", output)


def windows(root, output, app, seed):
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(app, "Xrose.exe")
        z.writestr("xrose.portable", "full" if seed else "lite")
        z.write(ROOT / "LICENSE", "LICENSE.txt")
        if seed:
            for file in sorted(seed.iterdir()):
                z.write(file, "resources/xrose-seed/" + file.name)


def linux_tree(root, app, fmt, seed):
    install = root / "usr"
    (install / "bin").mkdir(parents=True)
    shutil.copy2(app, install / "bin/xrose-app")
    desktop = "applications/io.xrose.Xrose.desktop"
    icon = "icons/hicolor/256x256/apps/xrose.png"
    for source, name in [(ASSETS / "io.xrose.Xrose.desktop", desktop),
                         (ASSETS / "icons/xrose.png", icon),
                         (ROOT / "LICENSE", "licenses/xrose/LICENSE")]:
        target = install / "share" / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
    resources = install / "share/xrose"
    resources.mkdir()
    (resources / "package-format").write_text(fmt + "\n")
    if seed:
        shutil.copytree(seed, resources / "xrose-seed")


def deb(root, output, version, arch):
    # Derive SONAME dependencies from the built executable on the build host.
    work = root.parent / "debian"
    work.mkdir(exist_ok=True)
    (work / "control").write_text("Source: xrose\nSection: net\nPriority: optional\nMaintainer: Project Xristina\n\nPackage: xrose\nArchitecture: any\nDescription: Xrose desktop client\n")
    result = run("dpkg-shlibdeps", "-O", "-e" + str(root / "usr/bin/xrose-app"),
                 cwd=root.parent, stdout=subprocess.PIPE, text=True)
    depends = next(line.split("=", 1)[1] for line in result.stdout.splitlines() if line.startswith("shlibs:Depends="))
    control = root / "DEBIAN/control"
    control.parent.mkdir()
    control.write_text(f"Package: xrose\nVersion: {version}\nArchitecture: {arch}\nSection: net\nPriority: optional\n"
                       f"Maintainer: Project Xristina\nDepends: {depends}\nDescription: Xrose desktop client\n")
    run("dpkg-deb", "--build", "--root-owner-group", root, output)


def appimage(root, output, arch, seed):
    # Deploy dependencies before adding the seed: linuxdeploy must not strip its CLI.
    machine = {"amd64": "x86_64", "arm64": "aarch64"}[arch]
    pins = json.loads((ROOT / "packaging/appimage-tools.json").read_text())
    tools = {}
    for name, pin in pins.items():
        filename = f"{name}-{machine}" if name == "runtime" else f"{name}-{machine}.AppImage"
        url = f"https://github.com/{pin['repo']}/releases/download/{pin['tag']}/{filename}"
        tool = fetch(url, pin[machine], filename)
        tool.chmod(0o755)
        tools[name] = tool
    env = dict(os.environ, ARCH=machine, NO_STRIP="1", APPIMAGE_EXTRACT_AND_RUN="1")
    share = root / "usr/share"
    run(tools["linuxdeploy"], "--appdir", root, "-e", root / "usr/bin/xrose-app",
        "-d", share / "applications/io.xrose.Xrose.desktop",
        "-i", share / "icons/hicolor/256x256/apps/xrose.png", env=env)
    if seed:
        shutil.copytree(seed, share / "xrose/xrose-seed")
    run(tools["appimagetool"], "--runtime-file", tools["runtime"], root, output, env=env)


def arch_package(root, output, version):
    work = root.parent
    recipe = (ROOT / "packaging/PKGBUILD").read_text().replace("@VERSION@", version.replace("-", "_"))
    (work / "PKGBUILD").write_text(recipe)
    if shutil.which("makepkg"):
        run("makepkg", "--nodeps", "--noconfirm", cwd=work)
    else:
        # Only the package assembly runs in Arch. Both variants use package name xrose
        # so pacman can upgrade a full installation with the lite artifact.
        run("docker", "run", "--rm", "-v", f"{work}:/work", "archlinux:base-devel", "bash", "-ec",
            'useradd -m builder; cp -r /work /home/builder/build; chown -R builder /home/builder/build; '
            'su builder -c "cd /home/builder/build && makepkg --nodeps --noconfirm"; '
            'cp /home/builder/build/*.pkg.tar.zst /work/')
    package, = work.glob("*.pkg.tar.zst")
    shutil.copyfile(package, output)


def main():
    parser = argparse.ArgumentParser(description="生成本平台的 Xrose full/lite 发布包")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    os_name = {"win32": "windows", "darwin": "macos", "linux": "linux"}[sys.platform]
    arch = {"x86_64": "amd64", "amd64": "amd64", "arm64": "arm64", "aarch64": "arm64"}[platform.machine().lower()]
    if os.getenv("XROSE_BUILD_ARCH", arch) != arch:
        raise ValueError("runner 架构与发布矩阵不一致")
    version, app, cli = binaries()
    formats = {"windows": ["zip"], "macos": ["dmg"], "linux": ["AppImage", "deb"]}[os_name]
    if os_name == "linux" and arch == "amd64":
        formats.append("pkg.tar.zst")
    with tempfile.TemporaryDirectory(dir=out) as tmp:
        tmp = Path(tmp)
        seed = tmp / "seed"
        seed_files(seed, os_name, arch, cli)
        for variant in ("full", "lite"):
            resources = seed if variant == "full" else None
            for ext in formats:
                name = f"Xrose-{version}-{os_name}-{arch}-{variant}.{ext}"
                stage = tmp / (variant + "-" + ext)
                stage.mkdir()
                root = stage / "root"
                root.mkdir()
                output = tmp / name
                if ext == "dmg":
                    darwin(root, output, app, version, resources)
                elif ext == "zip":
                    windows(root, output, app, resources)
                else:
                    fmt = "arch" if ext == "pkg.tar.zst" else ext
                    linux_tree(root, app, fmt, None if ext == "AppImage" else resources)
                    if ext == "AppImage":
                        appimage(root, output, arch, resources)
                    elif ext == "deb":
                        deb(root, output, version, arch)
                    else:
                        arch_package(root, output, version)
                output.replace(out / name)
                print(f"已生成 {name}", flush=True)


if __name__ == "__main__":
    main()
