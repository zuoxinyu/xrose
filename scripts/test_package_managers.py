"""Unit tests for scripts/package_managers.py."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import package_managers as pm


SAMPLE_SHA256SUMS = "\n".join(
    [
        f"{'a' * 64}  Xrose-0.1.0-linux-amd64-full.pkg.tar.zst",
        f"{'b' * 64}  Xrose-0.1.0-macos-arm64-full.dmg",
        f"{'c' * 64} *Xrose-0.1.0-macos-amd64-full.dmg",
        f"{'d' * 64}  Xrose-0.1.0-windows-amd64-full.zip",
        f"{'e' * 64}  Xrose-0.1.0-windows-arm64-full.zip",
    ]
)


class PackageManagersTests(unittest.TestCase):
    def test_parse_checksums_supports_binary_prefix_and_rejects_bad_digest(self) -> None:
        parsed = pm.parse_checksums(SAMPLE_SHA256SUMS)
        self.assertEqual(parsed["Xrose-0.1.0-macos-amd64-full.dmg"], "c" * 64)
        self.assertEqual(len(parsed), 5)

        with self.assertRaises(ValueError):
            pm.parse_checksums("not-a-sha  Xrose-0.1.0-macos-arm64-full.dmg\n")

    def test_generate_all_with_real_checksums(self) -> None:
        checksums = pm.parse_checksums(SAMPLE_SHA256SUMS)
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            written = pm.generate_all(
                output_dir=out,
                version="0.1.0",
                repo="example-org/xrose",
                checksums=checksums,
                targets={"aur", "homebrew", "scoop"},
                aur_maintainer="Example Maintainer <maintainer@example.invalid>",
                srcinfo_mode="fallback",
            )
            self.assertEqual(len(written), 4)

            pkgbuild = (out / "aur/PKGBUILD").read_text(encoding="utf-8")
            self.assertIn("# Maintainer: Example Maintainer <maintainer@example.invalid>", pkgbuild)
            self.assertIn("pkgname=xrose-bin", pkgbuild)
            self.assertIn("pkgver=0.1.0", pkgbuild)
            self.assertIn(f"sha256sums=('{'a' * 64}')", pkgbuild)

            srcinfo = (out / "aur/.SRCINFO").read_text(encoding="utf-8")
            self.assertIn("pkgbase = xrose-bin", srcinfo)
            self.assertIn(
                "https://github.com/example-org/xrose/releases/download/v0.1.0/Xrose-0.1.0-linux-amd64-full.pkg.tar.zst",
                srcinfo,
            )

            cask = (out / "homebrew/Casks/xrose.rb").read_text(encoding="utf-8")
            self.assertIn(f'sha256 arm:   "{"b" * 64}"', cask)
            self.assertIn(f'intel: "{"c" * 64}"', cask)
            self.assertIn('binary "#{appdir}/Xrose.app/Contents/Resources/xrose-seed/xrose-cli"', cask)

            scoop = json.loads((out / "scoop/bucket/xrose.json").read_text(encoding="utf-8"))
            self.assertEqual(scoop["version"], "0.1.0")
            self.assertEqual(scoop["architecture"]["64bit"]["hash"], "d" * 64)
            self.assertEqual(scoop["architecture"]["arm64"]["hash"], "e" * 64)
            self.assertEqual(scoop["persist"], "XroseData")
            self.assertEqual(scoop["bin"], ["Xrose.exe", "xrose-cli.cmd"])

    def test_prerelease_version_maps_hyphen_for_aur_pkgver(self) -> None:
        checksums = {
            "Xrose-0.1.0-rc.1-linux-amd64-full.pkg.tar.zst": "1" * 64,
        }
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            pm.generate_all(
                output_dir=out,
                version="0.1.0-rc.1",
                repo="example-org/xrose",
                checksums=checksums,
                targets={"aur"},
                srcinfo_mode="fallback",
            )
            pkgbuild = (out / "aur/PKGBUILD").read_text(encoding="utf-8")
            srcinfo = (out / "aur/.SRCINFO").read_text(encoding="utf-8")
            self.assertIn("_pkgver=0.1.0-rc.1", pkgbuild)
            self.assertIn("pkgver=0.1.0_rc.1", pkgbuild)
            self.assertIn("\tpkgver = 0.1.0_rc.1", srcinfo)
            self.assertIn("releases/download/v0.1.0-rc.1/Xrose-0.1.0-rc.1-linux-amd64-full.pkg.tar.zst", srcinfo)

    def test_placeholder_mode_uses_reserved_tokens(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            exit_code = pm.main(
                [
                    "--tag",
                    "v0.1.0",
                    "--output",
                    str(out),
                    "--srcinfo-mode",
                    "fallback",
                    "--allow-placeholders",
                ]
            )
            self.assertEqual(exit_code, 0)
            self.assertIn("<OWNER>/xrose", (out / "homebrew/Casks/xrose.rb").read_text(encoding="utf-8"))
            self.assertIn("<MAINTAINER> <<EMAIL>>", (out / "aur/PKGBUILD").read_text(encoding="utf-8"))
            scoop = json.loads((out / "scoop/bucket/xrose.json").read_text(encoding="utf-8"))
            self.assertEqual(scoop["architecture"]["64bit"]["hash"], "<SHA256_WINDOWS_AMD64_ZIP>")


if __name__ == "__main__":
    unittest.main()
