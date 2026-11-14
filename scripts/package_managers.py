"""Generate AUR, Homebrew Cask, and Scoop manifests from Xrose release assets."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_REPO_PLACEHOLDER = "<OWNER>/xrose"
DEFAULT_MAINTAINER_PLACEHOLDER = "<MAINTAINER> <<EMAIL>>"
DEFAULT_PKGBASE = "xrose-bin"
PACKAGE_DESC = "Xray and sing-box desktop client with EasyTier and Tailscale mesh"
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")

ARCH_DEPENDS = (
    "glibc",
    "gcc-libs",
    "gtk3",
    "libxkbcommon-x11",
    "libpipewire",
    "libxcb",
    "libxrandr",
    "mesa",
    "wayland",
    "openssl",
)


def detect_cargo_version() -> str:
    """Read xrose-app package version from Cargo workspace metadata."""
    output = subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"],
        cwd=ROOT,
        text=True,
    )
    metadata = json.loads(output)
    package = next(p for p in metadata["packages"] if p["name"] == "xrose-app")
    return package["version"]


def normalize_version(tag_or_version: str | None) -> str:
    """Strip optional leading 'v' from tag/version, or fall back to Cargo version."""
    raw = (tag_or_version or "").strip()
    if not raw:
        return detect_cargo_version()
    return raw[1:] if raw.startswith("v") else raw


def parse_checksums(text: str) -> dict[str, str]:
    """Parse sha256sum output lines ('<sha256>  <filename>' or '<sha256> *<filename>')."""
    checksums: dict[str, str] = {}
    for line_no, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line:
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            raise ValueError(f"SHA256SUMS 第 {line_no} 行格式无效：{raw}")
        digest, name = parts[0].lower(), parts[1].lstrip("*").strip()
        if not SHA256_RE.fullmatch(digest):
            raise ValueError(f"SHA256SUMS 第 {line_no} 行摘要格式无效：{digest}")
        checksums[Path(name).name] = digest
    return checksums


def asset_name(version: str, system: str, arch: str, ext: str, variant: str = "full") -> str:
    """Match release asset naming in scripts/release.py."""
    return f"Xrose-{version}-{system}-{arch}-{variant}.{ext}"


def resolve_digest(
    checksums: dict[str, str],
    filename: str,
    placeholder: str,
    allow_placeholders: bool,
) -> str:
    """Return verified SHA-256 for an asset, or a placeholder when previewing."""
    if filename in checksums:
        return checksums[filename]
    if allow_placeholders:
        return placeholder
    raise ValueError(f"校验文件中缺少发布产物：{filename}")


def render_pkgbuild(
    version: str,
    repo: str,
    sha256: str,
    pkgbase: str = DEFAULT_PKGBASE,
    maintainer: str | None = None,
) -> str:
    """Render AUR PKGBUILD for prebuilt Arch package extraction."""
    arch_ver = version.replace("-", "_")
    maintainer_line = f"# Maintainer: {maintainer or DEFAULT_MAINTAINER_PLACEHOLDER}\n"
    deps = " ".join(f"'{dep}'" for dep in ARCH_DEPENDS)
    filename = asset_name("${_pkgver}", "linux", "amd64", "pkg.tar.zst")
    url = f"https://github.com/{repo}/releases/download/v${{_pkgver}}/{filename}"
    return (
        f"{maintainer_line}"
        f"pkgname={pkgbase}\n"
        f"_pkgver={version}\n"
        f"pkgver={arch_ver}\n"
        "pkgrel=1\n"
        f'pkgdesc="{PACKAGE_DESC}"\n'
        "arch=('x86_64')\n"
        f'url="https://github.com/{repo}"\n'
        "license=('GPL-3.0-or-later')\n"
        f"depends=({deps})\n"
        "provides=('xrose')\n"
        "conflicts=('xrose')\n"
        "options=('!strip' '!debug')\n"
        f'source=("{filename}::{url}")\n'
        f"sha256sums=('{sha256}')\n"
        "\n"
        "package() {\n"
        '    cp -a "$srcdir/usr" "$pkgdir/"\n'
        "}\n"
    )


def render_srcinfo_fallback(
    version: str,
    repo: str,
    sha256: str,
    pkgbase: str = DEFAULT_PKGBASE,
) -> str:
    """Render AUR .SRCINFO deterministically when makepkg is unavailable."""
    arch_ver = version.replace("-", "_")
    filename = asset_name(version, "linux", "amd64", "pkg.tar.zst")
    url = f"https://github.com/{repo}/releases/download/v{version}/{filename}"
    lines = [
        f"pkgbase = {pkgbase}",
        f"\tpkgdesc = {PACKAGE_DESC}",
        f"\tpkgver = {arch_ver}",
        "\tpkgrel = 1",
        f"\turl = https://github.com/{repo}",
        "\tarch = x86_64",
        "\tlicense = GPL-3.0-or-later",
    ]
    lines.extend(f"\tdepends = {dep}" for dep in ARCH_DEPENDS)
    lines.extend(
        [
            "\tprovides = xrose",
            "\tconflicts = xrose",
            "\toptions = !strip",
            "\toptions = !debug",
            f"\tsource = {filename}::{url}",
            f"\tsha256sums = {sha256}",
            "",
            f"pkgname = {pkgbase}",
        ]
    )
    return "\n".join(lines) + "\n"


def generate_srcinfo_via_makepkg(pkgbuild_dir: Path, mode: str) -> str | None:
    """Run makepkg --printsrcinfo directly or inside archlinux:base-devel."""
    if mode in ("auto", "makepkg") and shutil.which("makepkg") and os.geteuid() != 0:
        return subprocess.check_output(
            ["makepkg", "--printsrcinfo"],
            cwd=pkgbuild_dir,
            text=True,
        )
    if mode == "makepkg":
        raise RuntimeError("未找到可用的 makepkg（或当前为 root 用户）")

    if mode in ("auto", "docker") and shutil.which("docker"):
        # Mirror scripts/release.py: makepkg refuses root, so run as an unprivileged user.
        return subprocess.check_output(
            [
                "docker",
                "run",
                "--rm",
                "-v",
                f"{pkgbuild_dir}:/work:ro",
                "archlinux:base-devel",
                "bash",
                "-ec",
                "useradd -m builder && cp /work/PKGBUILD /home/builder/PKGBUILD "
                "&& chown builder:builder /home/builder/PKGBUILD "
                '&& su builder -c "cd /home/builder && makepkg --printsrcinfo"',
            ],
            text=True,
        )
    if mode == "docker":
        raise RuntimeError("未找到 docker 命令，无法通过容器生成 .SRCINFO")
    return None


def render_homebrew_cask(
    version: str,
    repo: str,
    sha256_arm64: str,
    sha256_amd64: str,
) -> str:
    """Render Homebrew Cask definition (Casks/xrose.rb)."""
    return f'''cask "xrose" do
  arch arm: "arm64", intel: "amd64"

  version "{version}"
  sha256 arm:   "{sha256_arm64}",
         intel: "{sha256_amd64}"

  url "https://github.com/{repo}/releases/download/v#{{version}}/Xrose-#{{version}}-macos-#{{arch}}-full.dmg"
  name "Xrose"
  desc "{PACKAGE_DESC}"
  homepage "https://github.com/{repo}"

  livecheck do
    url :url
    strategy :github_latest
  end

  app "Xrose.app"
  binary "#{{appdir}}/Xrose.app/Contents/Resources/xrose-seed/xrose-cli"

  uninstall quit: "io.xrose.Xrose"

  zap trash: [
    "~/Library/Application Support/io.xrose.Xrose",
    "~/Library/Caches/io.xrose.Xrose",
    "~/Library/LaunchAgents/io.xrose.app.plist",
    "~/Library/Preferences/io.xrose.Xrose.plist",
  ]

  caveats <<~EOS
    If macOS Gatekeeper blocks first launch on unnotarized builds, open System Settings -> Privacy & Security and click "Open Anyway".
    If you previously installed the privileged helper, remove it via "xrose-cli helper uninstall" before zapping.
  EOS
end
'''


def render_scoop_manifest(
    version: str,
    repo: str,
    sha256_amd64: str,
    sha256_arm64: str,
) -> str:
    """Render Scoop bucket manifest (bucket/xrose.json)."""
    manifest = {
        "version": version,
        "description": PACKAGE_DESC,
        "homepage": f"https://github.com/{repo}",
        "license": "GPL-3.0-or-later",
        "architecture": {
            "64bit": {
                "url": f"https://github.com/{repo}/releases/download/v{version}/{asset_name(version, 'windows', 'amd64', 'zip')}",
                "hash": sha256_amd64,
            },
            "arm64": {
                "url": f"https://github.com/{repo}/releases/download/v{version}/{asset_name(version, 'windows', 'arm64', 'zip')}",
                "hash": sha256_arm64,
            },
        },
        # Create shim wrapper before Scoop links `bin`, keeping CLI synced with package upgrades
        # while honoring portable `XroseData` (unless the caller sets XROSE_HOME).
        "pre_install": [
            "New-Item -ItemType Directory -Force -Path $dir | Out-Null",
            "@('@echo off', 'setlocal', 'if not defined XROSE_HOME set \"XROSE_HOME=%~dp0XroseData\"', '\"%~dp0resources\\xrose-seed\\xrose-cli.exe\" %*') | Set-Content -Path \"$dir\\xrose-cli.cmd\" -Encoding Ascii",
        ],
        "bin": [
            "Xrose.exe",
            "xrose-cli.cmd",
        ],
        "shortcuts": [
            [
                "Xrose.exe",
                "Xrose",
            ]
        ],
        "persist": "XroseData",
        "post_install": [
            "$proc = Start-Process -FilePath \"$dir\\Xrose.exe\" -ArgumentList '--prepare-resources' -Wait -PassThru",
            "if ($proc.ExitCode -ne 0) { Write-Warning \"Xrose resource preparation exited with code $($proc.ExitCode)\" }",
        ],
        "checkver": "github",
        "autoupdate": {
            "architecture": {
                "64bit": {
                    "url": f"https://github.com/{repo}/releases/download/v$version/Xrose-$version-windows-amd64-full.zip",
                },
                "arm64": {
                    "url": f"https://github.com/{repo}/releases/download/v$version/Xrose-$version-windows-arm64-full.zip",
                },
            },
            "hash": {
                "url": "$baseurl/SHA256SUMS",
            },
        },
    }
    return json.dumps(manifest, indent=4) + "\n"


def generate_all(
    output_dir: Path,
    version: str,
    repo: str,
    checksums: dict[str, str],
    targets: set[str],
    aur_pkgbase: str = DEFAULT_PKGBASE,
    aur_maintainer: str | None = None,
    srcinfo_mode: str = "auto",
    allow_placeholders: bool = False,
) -> list[Path]:
    """Generate selected package manager manifests under output_dir."""
    written: list[Path] = []

    if "aur" in targets:
        linux_sha = resolve_digest(
            checksums,
            asset_name(version, "linux", "amd64", "pkg.tar.zst"),
            "<SHA256_LINUX_AMD64_PKG>",
            allow_placeholders,
        )
        aur_dir = output_dir / "aur"
        aur_dir.mkdir(parents=True, exist_ok=True)
        pkgbuild_path = aur_dir / "PKGBUILD"
        pkgbuild_path.write_text(
            render_pkgbuild(
                version=version,
                repo=repo,
                sha256=linux_sha,
                pkgbase=aur_pkgbase,
                maintainer=aur_maintainer,
            ),
            encoding="utf-8",
        )
        written.append(pkgbuild_path)

        srcinfo = None
        if srcinfo_mode != "fallback":
            with tempfile.TemporaryDirectory() as tmp:
                tmp_pkg = Path(tmp) / "PKGBUILD"
                tmp_pkg.write_text(pkgbuild_path.read_text(encoding="utf-8"), encoding="utf-8")
                srcinfo = generate_srcinfo_via_makepkg(Path(tmp), srcinfo_mode)
        if srcinfo is None:
            srcinfo = render_srcinfo_fallback(
                version=version,
                repo=repo,
                sha256=linux_sha,
                pkgbase=aur_pkgbase,
            )
        srcinfo_path = aur_dir / ".SRCINFO"
        srcinfo_path.write_text(srcinfo, encoding="utf-8")
        written.append(srcinfo_path)

    if "homebrew" in targets:
        mac_arm_sha = resolve_digest(
            checksums,
            asset_name(version, "macos", "arm64", "dmg"),
            "<SHA256_MACOS_ARM64_DMG>",
            allow_placeholders,
        )
        mac_amd_sha = resolve_digest(
            checksums,
            asset_name(version, "macos", "amd64", "dmg"),
            "<SHA256_MACOS_AMD64_DMG>",
            allow_placeholders,
        )
        cask_path = output_dir / "homebrew/Casks/xrose.rb"
        cask_path.parent.mkdir(parents=True, exist_ok=True)
        cask_path.write_text(
            render_homebrew_cask(
                version=version,
                repo=repo,
                sha256_arm64=mac_arm_sha,
                sha256_amd64=mac_amd_sha,
            ),
            encoding="utf-8",
        )
        written.append(cask_path)

    if "scoop" in targets:
        win_amd_sha = resolve_digest(
            checksums,
            asset_name(version, "windows", "amd64", "zip"),
            "<SHA256_WINDOWS_AMD64_ZIP>",
            allow_placeholders,
        )
        win_arm_sha = resolve_digest(
            checksums,
            asset_name(version, "windows", "arm64", "zip"),
            "<SHA256_WINDOWS_ARM64_ZIP>",
            allow_placeholders,
        )
        scoop_path = output_dir / "scoop/bucket/xrose.json"
        scoop_path.parent.mkdir(parents=True, exist_ok=True)
        scoop_path.write_text(
            render_scoop_manifest(
                version=version,
                repo=repo,
                sha256_amd64=win_amd_sha,
                sha256_arm64=win_arm_sha,
            ),
            encoding="utf-8",
        )
        written.append(scoop_path)

    return written


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="生成 AUR、Homebrew Cask 与 Scoop 分发清单")
    parser.add_argument("--tag", help="发布标签或版本号（例如 v0.1.0 或 0.1.0）；默认读取 Cargo 工作区版本")
    parser.add_argument(
        "--repo",
        default=os.getenv("XROSE_PACKAGE_REPO") or os.getenv("GITHUB_REPOSITORY") or "",
        help="GitHub 仓库（owner/repo）；未指定且启用占位模式时使用 <OWNER>/xrose",
    )
    parser.add_argument("--checksums", type=Path, help="SHA256SUMS 校验文件路径")
    parser.add_argument("--output", type=Path, default=ROOT / "dist/package-managers", help="输出目录")
    parser.add_argument(
        "--targets",
        nargs="+",
        choices=("aur", "homebrew", "scoop"),
        default=["aur", "homebrew", "scoop"],
        help="要生成的目标包管理器",
    )
    parser.add_argument("--aur-pkgbase", default=DEFAULT_PKGBASE, help="AUR 包名（默认 xrose-bin）")
    parser.add_argument("--aur-maintainer", default="", help="AUR PKGBUILD 顶部维护者信息")
    parser.add_argument(
        "--srcinfo-mode",
        choices=("auto", "makepkg", "docker", "fallback"),
        default="auto",
        help=".SRCINFO 生成方式（默认 auto）",
    )
    parser.add_argument(
        "--allow-placeholders",
        action="store_true",
        help="缺少仓库名或 SHA256SUMS 时保留占位符（用于预览或初始化外部仓库）",
    )
    args = parser.parse_args(argv)

    repo = args.repo.strip()
    if not repo:
        if not args.allow_placeholders:
            parser.error("请通过 --repo 指定 GitHub 仓库（owner/repo），或传入 --allow-placeholders 生成模板")
        repo = DEFAULT_REPO_PLACEHOLDER

    checksums: dict[str, str] = {}
    if args.checksums:
        checksums = parse_checksums(args.checksums.read_text(encoding="utf-8"))
    elif not args.allow_placeholders:
        parser.error("请通过 --checksums 指定 SHA256SUMS 文件，或传入 --allow-placeholders 生成模板")

    version = normalize_version(args.tag)
    maintainer = args.aur_maintainer.strip() or None
    written = generate_all(
        output_dir=args.output.resolve(),
        version=version,
        repo=repo,
        checksums=checksums,
        targets=set(args.targets),
        aur_pkgbase=args.aur_pkgbase,
        aur_maintainer=maintainer,
        srcinfo_mode=args.srcinfo_mode,
        allow_placeholders=args.allow_placeholders,
    )
    for path in written:
        print(f"已生成 {path}", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f"生成包管理器清单失败：{error}")
