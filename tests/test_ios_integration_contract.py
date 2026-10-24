"""End-to-end artifact boundaries for the private resource transfer and iOS package."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import struct
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
LOCAL = ROOT / ".local"
LOCAL.mkdir(exist_ok=True)
sys.path.insert(0, str(ROOT / "tools"))
import package_ios  # noqa: E402

PREPARE_SPEC = importlib.util.spec_from_file_location(
    "prepare_ipad_resources_contract", ROOT / "tools" / "prepare_ipad_resources.py"
)
prepare = importlib.util.module_from_spec(PREPARE_SPEC)
assert PREPARE_SPEC.loader is not None
PREPARE_SPEC.loader.exec_module(prepare)


def fake_png(width: int, height: int) -> bytes:
    return package_ios.PNG_SIGNATURE + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", width, height)


def make_prepared_resources(root: Path) -> Path:
    resources = root / "prepared"
    (resources / "assets/minecraft/sounds").mkdir(parents=True)
    (resources / "assets/minecraft/lang").mkdir(parents=True)
    (resources / "assets/minecraft/sounds.json").write_text("{}\n", encoding="utf-8")
    (resources / "assets/minecraft/sounds/test.ogg").write_bytes(b"synthetic audio payload")
    (resources / "assets/minecraft/lang/en_us.json").write_text("{}\n", encoding="utf-8")
    (resources / "facts.json").write_text("{}\n", encoding="utf-8")
    (resources / "block-tags.json").write_text("{}\n", encoding="utf-8")
    (resources / "facts.json").write_text("{}\n", encoding="utf-8")
    (resources / "horizon-resources.json").write_text(
        json.dumps({
            "schema": 7,
            "minecraft": "26.3",
            "data_version": 5023,
            "facts_sha256": hashlib.sha256((resources / "facts.json").read_bytes()).hexdigest(),
            "block_tags_sha256": hashlib.sha256((resources / "block-tags.json").read_bytes()).hexdigest(),
        }),
        encoding="utf-8",
    )
    return resources


class IosIntegrationContractTests(unittest.TestCase):
    def test_resource_transfer_and_app_package_remain_separate_artifacts(self) -> None:
        with tempfile.TemporaryDirectory(prefix="test-ios-contract-", dir=LOCAL) as temporary:
            root = Path(temporary)
            prepared = make_prepared_resources(root)
            transfer_path = root / ".local/ipad/horizon-resources-26.3.zip"
            file_count, byte_count, transfer_hash = prepare.package_resources(prepared, transfer_path)

            self.assertEqual(prepare.validate_transfer_archive(transfer_path), (file_count, byte_count))
            self.assertEqual(transfer_hash, hashlib.sha256(transfer_path.read_bytes()).hexdigest())
            with zipfile.ZipFile(transfer_path) as transfer_archive:
                transfer = json.loads(transfer_archive.read("horizon-transfer.json"))
                self.assertEqual(transfer["minecraft"], "26.3")
                self.assertEqual(transfer["resource_schema"], 7)
                self.assertEqual(
                    set(transfer["files"]),
                    set(transfer_archive.namelist()) - {"horizon-transfer.json"},
                )
                for name, record in transfer["files"].items():
                    content = transfer_archive.read(name)
                    self.assertEqual(record["size"], len(content))
                    self.assertEqual(record["sha256"], hashlib.sha256(content).hexdigest())

            binary = root / "horizon-client"
            binary.write_bytes(package_ios.MACHO64_LE + struct.pack("<I", package_ios.ARM64_CPU_TYPE) + b"fixture")
            icon = root / "horizon.png"
            icon.write_bytes(fake_png(512, 512))
            output = root / "device"
            args = SimpleNamespace(
                target="device",
                binary=binary,
                icon=icon,
                output_dir=output,
                commit="0123456789abcdef0123456789abcdef01234567",
                version="0.2.48",
                build_number="1",
                minimum_os="26.0",
                sdk_version="26.2",
                rustc_version="rustc fixture",
                cargo_version="cargo fixture",
                xcode_version="Xcode fixture",
                dsym=None,
                adhoc_sign=False,
            )

            def fake_sips(command: list[str], **_kwargs: object) -> SimpleNamespace:
                edge = int(command[5])
                Path(command[-1]).write_bytes(fake_png(edge, edge))
                return SimpleNamespace(returncode=0)

            with patch.object(package_ios.subprocess, "run", side_effect=fake_sips):
                ipa = package_ios.package(args)

            with zipfile.ZipFile(ipa) as app_archive:
                names = set(app_archive.namelist())
                app_files = {
                    f"Payload/Horizon.app/{package_ios.APP_NAME}",
                    "Payload/Horizon.app/Info.plist",
                    "Payload/Horizon.app/HorizonBuildInfo.json",
                    *(f"Payload/Horizon.app/{name}" for name in package_ios.ICON_SPECS),
                }
                self.assertEqual(names, app_files)
                forbidden_fragments = (
                    "assets/", "saves/", "horizon-resources.json", "horizon-transfer.json",
                    "server.jar", "client.jar", "session.lock", "minecraft/",
                )
                for fragment in forbidden_fragments:
                    self.assertFalse(any(fragment in name.lower() for name in names), fragment)
                self.assertEqual(
                    app_archive.read("Payload/Horizon.app/Horizon"), binary.read_bytes()
                )


if __name__ == "__main__":
    unittest.main()
