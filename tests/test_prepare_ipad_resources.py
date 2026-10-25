"""Synthetic tests for the private iPad resource transfer package."""

import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import warnings
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "tools" / "prepare_ipad_resources.py"
SPEC = importlib.util.spec_from_file_location("prepare_ipad_resources", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(prepare)
LOCAL = SCRIPT.parents[1] / ".local"
LOCAL.mkdir(exist_ok=True)


def temporary_root():
    return tempfile.TemporaryDirectory(prefix="test-ipad-resources-", dir=LOCAL)


def prepared_tree(root: Path) -> Path:
    resources = root / "prepared"
    (resources / "assets/minecraft/sounds").mkdir(parents=True)
    (resources / "assets/minecraft/lang").mkdir(parents=True)
    (resources / "assets/minecraft/sounds.json").write_text("{}\n", encoding="utf-8")
    (resources / "assets/minecraft/sounds/test.ogg").write_bytes(b"synthetic ogg data")
    (resources / "facts.json").write_text("{}\n", encoding="utf-8")
    (resources / "block-tags.json").write_text("{}\n", encoding="utf-8")
    (resources / "assets/minecraft/lang/en_us.json").write_text("{}\n", encoding="utf-8")
    (resources / "horizon-resources.json").write_text(json.dumps({
        "schema": 7,
        "minecraft": "26.3",
        "data_version": 5023,
        "facts_sha256": hashlib.sha256((resources / "facts.json").read_bytes()).hexdigest(),
        "block_tags_sha256": hashlib.sha256((resources / "block-tags.json").read_bytes()).hexdigest(),
        "resource_files": 3,
    }), encoding="utf-8")
    return resources


class PrepareIpadResourcesTests(unittest.TestCase):
    def test_sound_preparation_keeps_subcommand_before_root_options(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            command = prepare._horizon_prepare_command(
                root, "sounds", "--minecraft-dir", str(root / "minecraft")
            )
            if command[0] == "cargo":
                separator = command.index("--")
                self.assertEqual(command[separator + 1], "sounds")
            else:
                self.assertEqual(command[1], "sounds")
            self.assertEqual(command[-2:], ["--root", str(root)])

            executable = root / ("horizon-prepare.exe" if prepare.os.name == "nt" else "horizon-prepare")
            executable.touch()
            prebuilt_command = prepare._horizon_prepare_command(
                root, "sounds", "--minecraft-dir", str(root / "minecraft")
            )
            self.assertEqual(prebuilt_command[0], str(executable))
            self.assertEqual(prebuilt_command[1], "sounds")
            self.assertEqual(prebuilt_command[-2:], ["--root", str(root)])

    def test_package_has_hashes_for_every_resource_and_is_self_validating(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            resources = prepared_tree(root)
            source_before = {
                path.relative_to(resources).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in resources.rglob("*") if path.is_file()
            }
            output = root / ".local/ipad/resources.zip"
            count, total, digest = prepare.package_resources(resources, output)

            self.assertEqual(count, 6)
            self.assertGreater(total, 0)
            self.assertEqual(digest, hashlib.sha256(output.read_bytes()).hexdigest())
            self.assertEqual(prepare.validate_transfer_archive(output), (count, total))
            with zipfile.ZipFile(output) as archive:
                transfer = json.loads(archive.read("horizon-transfer.json"))
                self.assertEqual(transfer["minecraft"], "26.3")
                self.assertEqual(transfer["resource_schema"], 7)
                self.assertEqual(set(transfer["files"]), set(archive.namelist()) - {"horizon-transfer.json"})
                self.assertEqual(total, sum(record["size"] for record in transfer["files"].values()))
                for name, record in transfer["files"].items():
                    data = archive.read(name)
                    self.assertEqual(record["size"], len(data))
                    self.assertEqual(record["sha256"], hashlib.sha256(data).hexdigest())
            source_after = {
                path.relative_to(resources).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in resources.rglob("*") if path.is_file()
            }
            self.assertEqual(source_after, source_before)

    def test_output_must_not_overwrite_or_become_part_of_prepared_resources(self):
        with temporary_root() as tmp:
            resources = prepared_tree(Path(tmp))
            with self.assertRaisesRegex(ValueError, "outside the prepared resources folder"):
                prepare.package_resources(resources, resources / "transfer.zip")

    def test_output_symlink_does_not_redirect_replacement_to_its_target(self):
        if prepare.os.name == "nt":
            self.skipTest("creating filesystem symlinks may require Windows developer mode")
        with temporary_root() as tmp:
            root = Path(tmp)
            resources = prepared_tree(root)
            target = root / "keep.bin"
            target.write_bytes(b"preserve the symlink target")
            output = root / "resources.zip"
            try:
                output.symlink_to(target)
            except OSError as error:
                self.skipTest(f"filesystem symlinks are unavailable: {error}")

            with self.assertRaisesRegex(ValueError, "must not be a symbolic link"):
                prepare.package_resources(resources, output)
            self.assertEqual(target.read_bytes(), b"preserve the symlink target")

    def test_generated_manifest_hashes_must_match_required_resource_files(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            resources = prepared_tree(root)
            output = root / ".local/ipad/resources.zip"
            output.parent.mkdir(parents=True)
            output.write_bytes(b"previous archive")
            manifest_path = resources / "horizon-resources.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["facts_sha256"] = "0" * 64
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "hash does not match facts.json"):
                prepare.package_resources(resources, output)
            self.assertEqual(output.read_bytes(), b"previous archive")

    def test_invalid_prepared_manifest_keeps_previous_output(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            resources = prepared_tree(root)
            output = root / ".local/ipad/resources.zip"
            output.parent.mkdir(parents=True)
            output.write_bytes(b"previous archive")
            manifest_path = resources / "horizon-resources.json"
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            manifest["minecraft"] = "26.2"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "schema 7 for Minecraft 26.3"):
                prepare.package_resources(resources, output)
            self.assertEqual(output.read_bytes(), b"previous archive")

    def test_transfer_verification_detects_mutated_resource_bytes(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            output = root / "resources.zip"
            prepare.package_resources(prepared_tree(root), output)
            changed = root / "changed.zip"
            with zipfile.ZipFile(output) as source, zipfile.ZipFile(changed, "w") as target:
                for name in source.namelist():
                    data = source.read(name)
                    if name == "facts.json":
                        data += b"tampered"
                    target.writestr(name, data)
            with self.assertRaisesRegex(ValueError, "size mismatch|SHA-256 mismatch"):
                prepare.validate_transfer_archive(changed)

    def test_path_validation_rejects_traversal_windows_names_and_symlinks(self):
        for name in ("../escape", "/rooted", "C:/drive", "dir\\file", "NUL.txt"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                prepare.safe_relative_path(name)
        with temporary_root() as tmp:
            archive_path = Path(tmp) / "symlink.zip"
            info = zipfile.ZipInfo("link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            with zipfile.ZipFile(archive_path, "w") as archive:
                archive.writestr(info, "target")
            with zipfile.ZipFile(archive_path) as archive:
                with self.assertRaisesRegex(ValueError, "link or special"):
                    prepare._validate_member_set(archive.infolist())

    def test_preflight_rejects_duplicate_case_collisions_and_expansion_bombs(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            duplicate = root / "duplicate.zip"
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                with zipfile.ZipFile(duplicate, "w") as archive:
                    archive.writestr("same", b"first")
                    archive.writestr("same", b"second")
            with zipfile.ZipFile(duplicate) as archive:
                with self.assertRaisesRegex(ValueError, "duplicate archive path"):
                    prepare._validate_member_set(archive.infolist())

            case_collision = root / "case.zip"
            with zipfile.ZipFile(case_collision, "w") as archive:
                archive.writestr("Assets/file", b"one")
                archive.writestr("assets/other", b"two")
            with zipfile.ZipFile(case_collision) as archive:
                with self.assertRaisesRegex(ValueError, "case-colliding"):
                    prepare._validate_member_set(archive.infolist())

            bomb = root / "bomb.zip"
            with zipfile.ZipFile(bomb, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
                archive.writestr("zeros.bin", bytes(20 * 1024 * 1024))
            with zipfile.ZipFile(bomb) as archive:
                with self.assertRaisesRegex(ValueError, "compression ratio"):
                    prepare._validate_member_set(archive.infolist())

    def test_preflight_bounds_path_prefix_work(self):
        deep_prefix = "a/" * 2044
        infos = [zipfile.ZipInfo(f"{deep_prefix}f{index}") for index in range(5)]
        with self.assertRaisesRegex(ValueError, "path-prefix metadata"):
            prepare._validate_member_set(infos)

    def test_upstream_version_json_and_server_jar_must_match_pins(self):
        with temporary_root() as tmp:
            root = Path(tmp)
            minecraft = root / "minecraft"
            version_dir = minecraft / "versions/26.3"
            version_dir.mkdir(parents=True)
            server = root / "server.jar"
            server.write_bytes(b"pinned server jar")
            metadata_path = version_dir / "26.3.json"
            metadata_path.write_text(json.dumps({
                "id": "26.3",
                "downloads": {
                    "client": {"sha1": "client-pin"},
                    "server": {"sha1": hashlib.sha1(server.read_bytes()).hexdigest()},
                },
            }), encoding="utf-8")
            mc = {
                "version": "26.3",
                "manifest_sha1": hashlib.sha1(metadata_path.read_bytes()).hexdigest(),
                "accepted_manifest_sha1": [],
                "client_sha1": "client-pin",
                "server_sha1": hashlib.sha1(server.read_bytes()).hexdigest(),
            }
            (root / "research-lock.json").write_text(json.dumps({"minecraft": mc}), encoding="utf-8")

            prepare.verify_upstream_inputs(root, minecraft, server)
            server.write_bytes(b"not the pinned jar")
            with self.assertRaisesRegex(ValueError, "not the pinned Mojang 26.3 server"):
                prepare.verify_upstream_inputs(root, minecraft, server)

    def test_java_home_requires_java_and_compiler_to_both_report_jdk_25(self):
        with temporary_root() as tmp:
            java_home = Path(tmp) / "jdk"
            (java_home / "bin").mkdir(parents=True)
            (java_home / "bin" / ("java.exe" if prepare.os.name == "nt" else "java")).touch()
            (java_home / "bin" / ("javac.exe" if prepare.os.name == "nt" else "javac")).touch()
            java_25 = SimpleNamespace(returncode=0, stderr='openjdk version "25.0.1"', stdout="")
            javac_24 = SimpleNamespace(returncode=0, stderr="", stdout="javac 24.0.1")
            with patch.object(prepare.subprocess, "run", side_effect=[java_25, javac_24]):
                with self.assertRaisesRegex(ValueError, "contain javac 25"):
                    prepare._check_java25(java_home)


if __name__ == "__main__":
    unittest.main()
