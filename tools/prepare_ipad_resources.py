#!/usr/bin/env python3
"""Prepare and package private Minecraft 26.3 resources for Horizon on iPad.

The source install and downloaded server JAR are only read. Generated resources and the
transfer archive stay below this checkout's ignored .local/ directory.

For a normal preparation, horizon-prepare verifies the launcher version JSON and server JAR
against research-lock.json before it runs Mojang's exporters. For an existing prepared folder,
use --prepared PATH to package it without invoking Java or touching the Minecraft install.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import NoReturn
import zipfile

ROOT = Path(__file__).resolve().parents[1]
VERSION = "26.3"
RESOURCE_SCHEMA = 7
DATA_VERSION = 5023
TRANSFER_SCHEMA = 1
TRANSFER_NAME = "horizon-transfer.json"
RESOURCE_NAME = "horizon-resources.json"
MAX_ARCHIVE_BYTES = 4 * 1024 * 1024 * 1024
MAX_TOTAL_BYTES = 4 * 1024 * 1024 * 1024
MAX_FILE_BYTES = 512 * 1024 * 1024
MAX_FILES = 250_000
MAX_COMPRESSION_RATIO = 300
COPY_CHUNK = 1024 * 1024
SKIP_ROOT = {"classes", "client-classes", "export.log", "model-state-export.log", TRANSFER_NAME}


def fail(message: str) -> NoReturn:
    raise ValueError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(COPY_CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def sha1_file(path: Path) -> str:
    digest = hashlib.sha1()
    with path.open("rb") as stream:
        while chunk := stream.read(COPY_CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def lock_data(root: Path) -> dict:
    try:
        data = json.loads((root / "research-lock.json").read_text(encoding="utf-8"))
        mc = data["minecraft"]
        if mc["version"] != VERSION:
            fail(f"research-lock.json pins {mc['version']!r}, expected {VERSION}")
        return mc
    except (OSError, json.JSONDecodeError, KeyError, TypeError) as exc:
        raise ValueError(f"cannot read the pinned Minecraft metadata: {exc}") from exc


def default_minecraft_dir() -> Path:
    if os.environ.get("HORIZON_MINECRAFT_DIR"):
        return Path(os.environ["HORIZON_MINECRAFT_DIR"]).expanduser()
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        return Path(appdata) / ".minecraft" if appdata else Path.home() / ".minecraft"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "minecraft"
    return Path.home() / ".minecraft"


def verify_upstream_inputs(root: Path, minecraft: Path, server_jar: Path) -> dict:
    """Check the locally installed upstream manifest and user-supplied server JAR pins."""
    lock = lock_data(root)
    version_json = minecraft / "versions" / VERSION / f"{VERSION}.json"
    if not version_json.is_file():
        fail(f"missing {version_json}; install and launch Minecraft {VERSION} once first")
    try:
        metadata = json.loads(version_json.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"cannot read upstream version metadata {version_json}: {exc}") from exc
    expected_manifests = {lock.get("manifest_sha1"), *lock.get("accepted_manifest_sha1", [])}
    actual_manifest = sha1_file(version_json)
    if actual_manifest not in expected_manifests:
        fail(f"{version_json} is not a known Mojang 26.3 version manifest (SHA-1 {actual_manifest})")
    downloads = metadata.get("downloads", {})
    if (metadata.get("id") != VERSION
            or downloads.get("client", {}).get("sha1") != lock.get("client_sha1")
            or downloads.get("server", {}).get("sha1") != lock.get("server_sha1")):
        fail(f"{version_json} does not describe Horizon's pinned 26.3 release")
    if not server_jar.is_file():
        fail(f"server JAR does not exist: {server_jar}")
    actual_server = sha1_file(server_jar)
    if actual_server != lock.get("server_sha1"):
        fail(f"server JAR is not the pinned Mojang 26.3 server (SHA-1 {actual_server})")
    return lock


def safe_relative_path(name: str, is_dir: bool = False) -> str:
    if not name or "\\" in name or "\x00" in name or name.startswith("/"):
        fail(f"unsafe archive path: {name!r}")
    path = name[:-1] if is_dir and name.endswith("/") else name
    if not path or len(path) > 4096:
        fail(f"unsafe archive path: {name!r}")
    parts = path.split("/")
    for part in parts:
        if (part in {"", ".", ".."} or ":" in part or part.endswith((".", " "))
                or any(ord(ch) < 32 or ch in '<>"|?*' for ch in part)):
            fail(f"unsafe archive path: {name!r}")
        stem = part.split(".", 1)[0].upper()
        if (stem in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"}
                or re.fullmatch(r"(?:COM|LPT)[1-9]", stem)):
            fail(f"unsafe Windows path component in archive: {part!r}")
    return "/".join(parts)


def _zip_is_dir(info: zipfile.ZipInfo) -> bool:
    mode = (info.external_attr >> 16) & 0xFFFF
    return info.is_dir() or stat.S_IFMT(mode) == stat.S_IFDIR


def _validate_member_set(infos: list[zipfile.ZipInfo]) -> tuple[dict[str, zipfile.ZipInfo], int]:
    if len(infos) > MAX_FILES:
        fail(f"archive has too many entries (limit {MAX_FILES})")
    actual: dict[str, zipfile.ZipInfo] = {}
    folded: dict[str, str] = {}
    kinds: dict[str, bool] = {}
    total = 0
    for info in infos:
        mode = (info.external_attr >> 16) & 0xFFFF
        file_type = stat.S_IFMT(mode)
        is_dir = info.is_dir() or file_type == stat.S_IFDIR
        name = safe_relative_path(info.filename, is_dir)
        canonical = name.lower()
        if canonical in actual:
            fail(f"duplicate archive path: {info.filename}")
        actual[canonical] = info
        kinds[canonical] = is_dir
        for index in range(1, len(name.split("/")) + 1):
            prefix = "/".join(name.split("/")[:index])
            key = prefix.lower()
            prior = folded.get(key)
            if prior is not None and prior != prefix:
                fail(f"case-colliding archive paths: {prior!r} and {prefix!r}")
            folded[key] = prefix
        if file_type not in (0, stat.S_IFREG, stat.S_IFDIR) or stat.S_ISLNK(mode):
            fail(f"archive contains a link or special file: {info.filename}")
        if is_dir:
            if info.file_size != 0:
                fail(f"directory entry contains data: {info.filename}")
            continue
        if info.file_size < 0 or info.file_size > MAX_FILE_BYTES:
            fail(f"archive file exceeds size limit: {info.filename}")
        total += info.file_size
        if total > MAX_TOTAL_BYTES:
            fail("archive exceeds total uncompressed size limit")
        if info.file_size > 16 * 1024 * 1024:
            if info.compress_size == 0 or info.file_size > info.compress_size * MAX_COMPRESSION_RATIO:
                fail(f"suspicious compression ratio: {info.filename}")
    for key, is_dir in kinds.items():
        if is_dir:
            continue
        parts = key.split("/")
        for index in range(1, len(parts)):
            parent = "/".join(parts[:index])
            if parent in kinds and not kinds[parent]:
                fail(f"archive file is also used as a parent directory: {parent}")
    return actual, total


def _read_zip_file(archive: zipfile.ZipFile, info: zipfile.ZipInfo, limit: int) -> bytes:
    if info.file_size > limit:
        fail(f"archive metadata is too large: {info.filename}")
    with archive.open(info, "r") as stream:
        data = stream.read(limit + 1)
        if len(data) > limit or stream.read(1):
            fail(f"archive metadata exceeds size limit: {info.filename}")
    return data


def validate_transfer_archive(path: Path) -> tuple[int, int]:
    """Read and verify every member before a package is promoted into .local/."""
    if path.stat().st_size > MAX_ARCHIVE_BYTES:
        fail("transfer archive exceeds compressed size limit")
    try:
        with zipfile.ZipFile(path) as archive:
            members, total = _validate_member_set(archive.infolist())
            transfer_info = members.get(TRANSFER_NAME.lower())
            resource_info = members.get(RESOURCE_NAME.lower())
            if transfer_info is None or _zip_is_dir(transfer_info):
                fail(f"transfer archive has no root {TRANSFER_NAME}")
            if transfer_info.filename != TRANSFER_NAME:
                fail(f"transfer archive must use the exact root path {TRANSFER_NAME}")
            if resource_info is None or _zip_is_dir(resource_info):
                fail(f"transfer archive has no root {RESOURCE_NAME}")
            if resource_info.filename != RESOURCE_NAME:
                fail(f"transfer archive must use the exact root path {RESOURCE_NAME}")
            try:
                transfer = json.loads(_read_zip_file(archive, transfer_info, 16 * 1024 * 1024))
            except json.JSONDecodeError as exc:
                raise ValueError(f"invalid {TRANSFER_NAME}: {exc}") from exc
            if (not isinstance(transfer, dict)
                    or set(transfer) != {"schema", "minecraft", "resource_schema", "files"}
                    or type(transfer.get("schema")) is not int
                    or transfer.get("schema") != TRANSFER_SCHEMA
                    or transfer.get("minecraft") != VERSION
                    or type(transfer.get("resource_schema")) is not int
                    or transfer.get("resource_schema") != RESOURCE_SCHEMA
                    or not isinstance(transfer.get("files"), dict)):
                fail("transfer manifest is not Horizon schema 1 for Minecraft 26.3 resources schema 7")
            expected_files = transfer["files"]
            actual_file_keys = {key for key, info in members.items()
                                if not _zip_is_dir(info) and key != TRANSFER_NAME.lower()}
            actual_names = {key: info.filename for key, info in members.items()
                            if not _zip_is_dir(info) and key != TRANSFER_NAME.lower()}
            required_files = {
                RESOURCE_NAME.lower(),
                "facts.json",
                "block-tags.json",
                "assets/minecraft/sounds.json",
            }
            if (not required_files.issubset(actual_file_keys)
                    or any(actual_names.get(name.lower()) != name for name in required_files)
                    or not any(
                    name.startswith("assets/minecraft/sounds/") and name.endswith(".ogg")
                    for name in actual_file_keys)):
                fail("transfer archive is missing required prepared resources or sounds")
            expected_keys: set[str] = set()
            actual_total = 0
            for name, record in expected_files.items():
                safe = safe_relative_path(name)
                key = safe.lower()
                if key in expected_keys or key == TRANSFER_NAME.lower():
                    fail(f"duplicate or reserved path in transfer hashes: {name}")
                expected_keys.add(key)
                if not isinstance(record, dict) or set(record) != {"size", "sha256"}:
                    fail(f"invalid transfer hash record for {name}")
                if (not isinstance(record["size"], int) or isinstance(record["size"], bool)
                        or record["size"] < 0
                        or not isinstance(record["sha256"], str)
                        or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"])):
                    fail(f"invalid transfer hash record for {name}")
                info = members.get(key)
                if info is None or info.is_dir():
                    fail(f"transfer archive is missing hashed file {name}")
                if info.file_size != record["size"]:
                    fail(f"size mismatch for {name}")
                digest = hashlib.sha256()
                count = 0
                with archive.open(info, "r") as stream:
                    while chunk := stream.read(COPY_CHUNK):
                        count += len(chunk)
                        if count > record["size"] or count > MAX_FILE_BYTES:
                            fail(f"archive member expanded beyond its size limit: {name}")
                        digest.update(chunk)
                actual_total += count
                if actual_total > MAX_TOTAL_BYTES:
                    fail("transfer archive expanded beyond total size limit")
                if count != record["size"] or digest.hexdigest() != record["sha256"]:
                    fail(f"SHA-256 mismatch for {name}")
            if actual_file_keys != expected_keys:
                fail("transfer manifest file list does not match the archive")
            resource_bytes = _read_zip_file(archive, resource_info, 16 * 1024 * 1024)
            try:
                resources = json.loads(resource_bytes)
            except json.JSONDecodeError as exc:
                raise ValueError(f"invalid {RESOURCE_NAME}: {exc}") from exc
            if (not isinstance(resources, dict) or resources.get("schema") != RESOURCE_SCHEMA
                    or resources.get("minecraft") != VERSION
                    or resources.get("data_version") != DATA_VERSION):
                fail("prepared resources are not Horizon schema 7 for Minecraft 26.3")
            for manifest_key, file_name in (("facts_sha256", "facts.json"),
                                            ("block_tags_sha256", "block-tags.json")):
                expected = resources.get(manifest_key)
                record = expected_files.get(file_name)
                if (not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected)
                        or not isinstance(record, dict) or record.get("sha256") != expected):
                    fail(f"{RESOURCE_NAME} hash does not match {file_name}")
            return len(expected_files), actual_total
    except zipfile.BadZipFile as exc:
        raise ValueError(f"invalid transfer ZIP: {exc}") from exc


def _runtime_files(root: Path) -> list[tuple[str, Path]]:
    files: list[tuple[str, Path]] = []
    total = 0
    for current, dirs, names in os.walk(root, followlinks=False):
        current_path = Path(current)
        rel_dir = current_path.relative_to(root)
        kept_dirs = []
        for name in sorted(dirs):
            path = current_path / name
            if not rel_dir.parts and name in SKIP_ROOT:
                continue
            if name.endswith("-export") or name.endswith("-classes"):
                continue
            if path.is_symlink():
                fail(f"prepared resources contain a symbolic link: {path}")
            kept_dirs.append(name)
        dirs[:] = kept_dirs
        for name in sorted(names):
            path = current_path / name
            if not rel_dir.parts and (name in SKIP_ROOT or name.endswith("-export.log")):
                continue
            if path.is_symlink() or not path.is_file():
                fail(f"prepared resources contain a link or special file: {path}")
            relative = path.relative_to(root).as_posix()
            safe_relative_path(relative)
            size = path.stat().st_size
            if size > MAX_FILE_BYTES:
                fail(f"prepared resource exceeds per-file size limit: {relative}")
            total += size
            if total > MAX_TOTAL_BYTES:
                fail("prepared resources exceed total size limit")
            files.append((relative, path))
            if len(files) > MAX_FILES:
                fail(f"prepared resources contain more than {MAX_FILES} files")
    if not any(name == RESOURCE_NAME for name, _ in files):
        fail(f"missing {RESOURCE_NAME} in {root}")
    if not any(name == "assets/minecraft/sounds.json" for name, _ in files):
        fail("missing assets/minecraft/sounds.json; run horizon-prepare sounds")
    if not any(name.startswith("assets/minecraft/sounds/") and name.endswith(".ogg")
               for name, _ in files):
        fail("missing Minecraft sound files; run horizon-prepare sounds")
    return files


def validate_prepared_folder(path: Path) -> list[tuple[str, Path]]:
    if not path.is_dir() or path.is_symlink():
        fail(f"prepared resources are not a real directory: {path}")
    files = _runtime_files(path)
    try:
        manifest = json.loads((path / RESOURCE_NAME).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid {RESOURCE_NAME}: {exc}") from exc
    if (not isinstance(manifest, dict) or manifest.get("schema") != RESOURCE_SCHEMA
            or manifest.get("minecraft") != VERSION or manifest.get("data_version") != DATA_VERSION):
        fail("prepared resources are not Horizon schema 7 for Minecraft 26.3")
    if not (path / "facts.json").is_file() or not (path / "block-tags.json").is_file():
        fail("prepared resources are incomplete (facts.json or block-tags.json is missing)")
    for manifest_key, file_name in (("facts_sha256", "facts.json"),
                                    ("block_tags_sha256", "block-tags.json")):
        expected = manifest.get(manifest_key)
        if (not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected)
                or sha256_file(path / file_name) != expected):
            fail(f"{RESOURCE_NAME} hash does not match {file_name}")
    return files


def package_resources(prepared: Path, output: Path) -> tuple[int, int, str]:
    """Build a verified transfer ZIP beside output, preserving any previous output on failure."""
    prepared = prepared.resolve()
    output = output.resolve()
    try:
        output.relative_to(prepared)
    except ValueError:
        pass
    else:
        fail("transfer archive output must be outside the prepared resources folder")
    files = validate_prepared_folder(prepared)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.{os.getpid()}.partial")
    hashes: dict[str, dict[str, object]] = {}
    try:
        temporary.unlink(missing_ok=True)
        with zipfile.ZipFile(temporary, "w", allowZip64=True) as archive:
            for name, source in files:
                method = zipfile.ZIP_STORED if source.suffix.lower() in {
                    ".zip", ".ogg", ".png", ".jpg", ".jpeg", ".webp", ".gz", ".jar"
                } else zipfile.ZIP_DEFLATED
                info = zipfile.ZipInfo(name)
                info.compress_type = method
                info.external_attr = (stat.S_IFREG | 0o644) << 16
                digest = hashlib.sha256()
                size = 0
                with source.open("rb") as src, archive.open(info, "w", force_zip64=True) as dst:
                    while chunk := src.read(COPY_CHUNK):
                        size += len(chunk)
                        digest.update(chunk)
                        dst.write(chunk)
                hashes[name] = {"size": size, "sha256": digest.hexdigest()}
            transfer = {
                "schema": TRANSFER_SCHEMA,
                "minecraft": VERSION,
                "resource_schema": RESOURCE_SCHEMA,
                "files": hashes,
            }
            info = zipfile.ZipInfo(TRANSFER_NAME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            archive.writestr(info, json.dumps(transfer, sort_keys=True, separators=(",", ":")) + "\n")
        if temporary.stat().st_size > MAX_ARCHIVE_BYTES:
            fail("transfer archive exceeds compressed size limit")
        file_count, total = validate_transfer_archive(temporary)
        os.replace(temporary, output)
        return file_count, total, sha256_file(output)
    finally:
        temporary.unlink(missing_ok=True)


def _check_java25(java_home: Path) -> None:
    executable = java_home / "bin" / ("java.exe" if os.name == "nt" else "java")
    if not executable.is_file():
        fail(f"no java executable under --java-home {java_home}")
    result = subprocess.run([str(executable), "-version"], text=True, capture_output=True)
    output = result.stderr + result.stdout
    match = re.search(r'version "?(\d+)', output)
    if result.returncode or not match or int(match.group(1)) != 25:
        fail(f"--java-home must point to JDK 25; {executable} reported {output.strip() or result.returncode}")
    compiler = java_home / "bin" / ("javac.exe" if os.name == "nt" else "javac")
    if not compiler.is_file():
        fail(f"--java-home must be a JDK 25 (javac is missing): {java_home}")
    compiler_result = subprocess.run([str(compiler), "-version"], text=True, capture_output=True)
    compiler_output = compiler_result.stderr + compiler_result.stdout
    compiler_match = re.search(r"javac\s+(\d+)", compiler_output)
    if (compiler_result.returncode or not compiler_match
            or int(compiler_match.group(1)) != 25):
        fail(f"--java-home must contain javac 25; {compiler} reported "
             f"{compiler_output.strip() or compiler_result.returncode}")


def _within_local(path: Path, root: Path) -> Path:
    local = (root / ".local").resolve()
    resolved = path.resolve()
    try:
        resolved.relative_to(local)
    except ValueError:
        fail(f"output must stay under ignored .local/: {resolved}")
    return resolved


def _run_preparation(root: Path, args: argparse.Namespace) -> Path:
    minecraft = (args.minecraft_dir or default_minecraft_dir()).expanduser().resolve()
    server = args.server_jar
    if server is None and os.environ.get("HORIZON_SERVER_JAR"):
        server = Path(os.environ["HORIZON_SERVER_JAR"])
    if server is None:
        cached = root / ".local" / "minecraft" / VERSION / "jars" / "server.jar"
        if cached.is_file():
            server = cached
    if server is None:
        fail("pass --server-jar PATH to your own pinned Minecraft 26.3 server JAR")
    server = server.expanduser().resolve()
    verify_upstream_inputs(root, minecraft, server)

    environment = os.environ.copy()
    if args.java_home:
        java_home = args.java_home.expanduser().resolve()
        _check_java25(java_home)
        environment["JAVA_HOME"] = str(java_home)
    wrapper = root / "tools" / "prepare_native_client.py"
    command = [sys.executable, str(wrapper), "--prepare", "--minecraft-dir", str(minecraft),
               "--server-jar", str(server)]
    subprocess.run(command, cwd=root, env=environment, check=True)
    subprocess.run([sys.executable, str(wrapper), "sounds", "--minecraft-dir", str(minecraft)],
                   cwd=root, env=environment, check=True)
    return root / ".local" / "horizon-client" / VERSION


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--minecraft-dir", type=Path, help="existing Minecraft 26.3 game directory (read-only)")
    parser.add_argument("--server-jar", type=Path, help="your own Minecraft 26.3 server JAR")
    parser.add_argument("--java-home", type=Path, help="JDK 25 home used for preparation")
    parser.add_argument("--prepared", type=Path, help="package an already-prepared resources folder; skip Java")
    parser.add_argument("--out", type=Path, help="output ZIP (must stay under this checkout's .local/)")
    args = parser.parse_args(argv)
    root = ROOT.resolve()
    output = _within_local(args.out or root / ".local" / "ipad" / "horizon-resources-26.3.zip", root)
    try:
        if args.prepared:
            prepared = args.prepared.expanduser().resolve()
        else:
            prepared = _run_preparation(root, args)
        count, total, digest = package_resources(prepared, output)
        print(f"Prepared {count} resource files ({total / (1024 * 1024):.1f} MiB)")
        print(f"{output} ({output.stat().st_size / (1024 * 1024):.1f} MiB)")
        print(f"SHA-256 {digest}")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        print(f"prepare_ipad_resources: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
