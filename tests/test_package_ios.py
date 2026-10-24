from __future__ import annotations

import json
import plistlib
import struct
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
import package_ios  # noqa: E402


def fake_png(width: int, height: int) -> bytes:
    return (
        package_ios.PNG_SIGNATURE
        + struct.pack(">I", 13)
        + b"IHDR"
        + struct.pack(">II", width, height)
    )


def make_macho(path: Path, cpu_type: int = package_ios.ARM64_CPU_TYPE) -> None:
    path.write_bytes(package_ios.MACHO64_LE + struct.pack("<I", cpu_type) + b"synthetic Mach-O fixture")


class PackageIosTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.binary = self.root / "horizon-client"
        self.icon = self.root / "horizon.png"
        make_macho(self.binary)
        self.icon.write_bytes(fake_png(512, 512))

    def tearDown(self) -> None:
        self.temp.cleanup()

    def args(self, target: str, *, adhoc_sign: bool = False) -> SimpleNamespace:
        return SimpleNamespace(
            target=target,
            binary=self.binary,
            icon=self.icon,
            output_dir=self.root / target,
            commit="0123456789abcdef0123456789abcdef01234567",
            version="0.2.48",
            build_number="42",
            minimum_os="26.0",
            sdk_version="26.2",
            rustc_version="rustc 1.90.0 (fixture)",
            cargo_version="cargo 1.90.0 (fixture)",
            xcode_version="Xcode 26.2 (fixture)",
            dsym=None,
            adhoc_sign=adhoc_sign,
        )

    @staticmethod
    def fake_tools(command: list[str], **_kwargs: object) -> SimpleNamespace:
        if command[0] == "sips":
            edge = int(command[5])
            Path(command[-1]).write_bytes(fake_png(edge, edge))
        elif command[0] == "codesign":
            app = Path(command[-1])
            signature = app / "_CodeSignature"
            signature.mkdir()
            (signature / "CodeResources").write_text("ad-hoc simulator signature", encoding="utf-8")
        else:
            raise AssertionError(f"unexpected tool invocation: {command}")
        return SimpleNamespace(returncode=0)

    def test_device_ipa_is_unsigned_metadata_bound_and_contains_no_game_data(self) -> None:
        args = self.args("device")
        with patch.object(package_ios.subprocess, "run", side_effect=self.fake_tools):
            artifact = package_ios.package(args)

        with zipfile.ZipFile(artifact) as archive:
            names = set(archive.namelist())
            self.assertIn("Payload/Horizon.app/Horizon", names)
            self.assertIn("Payload/Horizon.app/Info.plist", names)
            self.assertIn("Payload/Horizon.app/HorizonBuildInfo.json", names)
            self.assertIn("Payload/Horizon.app/AppIcon60x60@3x.png", names)
            self.assertFalse(any("_CodeSignature" in name or "embedded.mobileprovision" in name for name in names))
            self.assertFalse(any(name.startswith(("Payload/Horizon.app/assets/", "Payload/Horizon.app/saves/")) for name in names))
            info = plistlib.loads(archive.read("Payload/Horizon.app/Info.plist"))
            self.assertEqual(info["CFBundleIdentifier"], "dev.lyszq.horizon")
            self.assertEqual(info["MinimumOSVersion"], "26.0")
            self.assertEqual(info["UIDeviceFamily"], [1, 2])
            metadata = json.loads(archive.read("Payload/Horizon.app/HorizonBuildInfo.json"))
            self.assertEqual(metadata["commit"], args.commit)
            self.assertEqual(metadata["target"], "aarch64-apple-ios")
            self.assertEqual(metadata["sha256"]["rust_macho"], package_ios.sha256(self.binary))
        self.assertEqual((args.output_dir / "Horizon.ipa.sha256").read_text().split()[0], package_ios.sha256(artifact))

    def test_simulator_package_is_adhoc_signed_and_targets_apple_silicon_simulator(self) -> None:
        args = self.args("simulator", adhoc_sign=True)
        with patch.object(package_ios.subprocess, "run", side_effect=self.fake_tools) as tools:
            artifact = package_ios.package(args)

        tools.assert_any_call(
            ["codesign", "--force", "--sign", "-", "--timestamp=none", str(args.output_dir.resolve() / "Horizon.app")],
            check=True,
        )
        with zipfile.ZipFile(artifact) as archive:
            names = set(archive.namelist())
            self.assertIn("Horizon.app/_CodeSignature/CodeResources", names)
            metadata = json.loads(archive.read("Horizon.app/HorizonBuildInfo.json"))
            self.assertEqual(metadata["platform"], "simulator")
            self.assertEqual(metadata["target"], "aarch64-apple-ios-sim")
            info = plistlib.loads(archive.read("Horizon.app/Info.plist"))
            self.assertEqual(info["DTPlatformName"], "iphonesimulator")

    def test_rejects_wrong_architecture_before_writing_an_app(self) -> None:
        make_macho(self.binary, cpu_type=0x01000007)
        args = self.args("device")
        with self.assertRaisesRegex(ValueError, "expected arm64 Mach-O"):
            package_ios.package(args)
        self.assertFalse((args.output_dir / "Horizon.app").exists())

    def test_rejects_device_signing_and_non_26_minimum(self) -> None:
        with patch.object(package_ios.subprocess, "run", side_effect=self.fake_tools):
            with self.assertRaisesRegex(ValueError, "must remain unsigned"):
                package_ios.package(self.args("device", adhoc_sign=True))
            args = self.args("simulator")
            args.minimum_os = "25.0"
            with self.assertRaisesRegex(ValueError, "minimum iOS 26.0"):
                package_ios.package(args)


if __name__ == "__main__":
    unittest.main()
