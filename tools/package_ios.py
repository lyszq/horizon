#!/usr/bin/env python3
"""Package the Rust client as an unsigned iOS IPA or an ad-hoc signed simulator app."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
APP_NAME = "Horizon"
BUNDLE_ID = "dev.lyszq.horizon"
MINIMUM_IOS = "26.0"
ICON_SOURCE = ROOT / "packaging/icons/hicolor/512x512/apps/horizon.png"
LICENSE_SOURCES = {
    "Horizon-MIT.txt": ROOT / "LICENSE",
    "OFL-Geist.txt": ROOT / "crates/horizon-theme/fonts/OFL-Geist.txt",
    "OFL-PixelifySans.txt": ROOT / "crates/horizon-theme/fonts/OFL-PixelifySans.txt",
    "LICENSE-pixelarticons.txt": ROOT / "crates/horizon-theme/icons/pixel/LICENSE-pixelarticons.txt",
}
ICON_SPECS = {
    "AppIcon60x60@2x.png": 120,
    "AppIcon60x60@3x.png": 180,
    "AppIcon76x76~ipad.png": 76,
    "AppIcon76x76@2x~ipad.png": 152,
    "AppIcon83.5x83.5@2x~ipad.png": 167,
}
MACHO64_LE = b"\xcf\xfa\xed\xfe"
ARM64_CPU_TYPE = 0x0100000C
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def tree_sha256(root: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(bytes.fromhex(sha256(path)))
    return digest.hexdigest()


def png_dimensions(path: Path) -> tuple[int, int]:
    with path.open("rb") as stream:
        header = stream.read(24)
    if len(header) != 24 or header[:8] != PNG_SIGNATURE or header[12:16] != b"IHDR":
        raise ValueError(f"not a PNG image: {path}")
    return struct.unpack(">II", header[16:24])


def validate_arm64_macho(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(8)
    if len(header) < 8 or header[:4] != MACHO64_LE:
        raise ValueError(f"expected a 64-bit little-endian Mach-O executable: {path}")
    cpu_type = struct.unpack("<I", header[4:8])[0]
    if cpu_type != ARM64_CPU_TYPE:
        raise ValueError(f"expected arm64 Mach-O (cpu type 0x{ARM64_CPU_TYPE:08x}), got 0x{cpu_type:08x}")


def make_icons(source: Path, app: Path) -> dict[str, str]:
    png_dimensions(source)
    hashes: dict[str, str] = {}
    for filename, edge in ICON_SPECS.items():
        destination = app / filename
        if png_dimensions(source) == (edge, edge):
            shutil.copyfile(source, destination)
        else:
            subprocess.run(
                ["sips", "-s", "format", "png", "-z", str(edge), str(edge), str(source), "--out", str(destination)],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
        if png_dimensions(destination) != (edge, edge):
            raise ValueError(f"icon renderer produced the wrong dimensions for {filename}")
        hashes[filename] = sha256(destination)
    return hashes


def app_info(platform: str, version: str, build_number: str, sdk_version: str) -> dict[str, Any]:
    icon_names = [Path(name).stem for name in ICON_SPECS]
    ipad_icons = [Path(name).stem for name in ICON_SPECS if "ipad" in name]
    primary_icon = {"CFBundleIconFiles": icon_names, "UIPrerenderedIcon": False}
    return {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": APP_NAME,
        "NSHumanReadableCopyright": "Copyright (c) 2026 Ludovic. Horizon is MIT licensed. Not an official Minecraft product; not approved by or associated with Mojang or Microsoft.",
        "CFBundleExecutable": APP_NAME,
        "CFBundleIcons": {"CFBundlePrimaryIcon": primary_icon},
        "CFBundleIcons~ipad": {"CFBundlePrimaryIcon": {"CFBundleIconFiles": ipad_icons, "UIPrerenderedIcon": False}},
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": APP_NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": build_number,
        "DTPlatformName": "iphoneos" if platform == "device" else "iphonesimulator",
        "DTPlatformVersion": sdk_version,
        "DTSDKName": f"{'iphoneos' if platform == 'device' else 'iphonesimulator'}{sdk_version}",
        "LSRequiresIPhoneOS": True,
        "UILaunchScreen": {},
        "UIFileSharingEnabled": True,
        "LSSupportsOpeningDocumentsInPlace": True,
        "NSLocalNetworkUsageDescription": "Connect to Minecraft and Horizon servers on your local network.",
        "NSBonjourServices": ["_minecraft._tcp"],
        "GCSupportsControllerUserInteraction": True,
        "GCSupportedGameControllers": [{"ProfileName": "ExtendedGamepad"}],
        "MinimumOSVersion": MINIMUM_IOS,
        "UIRequiredDeviceCapabilities": ["arm64", "metal"],
        "UIDeviceFamily": [1, 2],
        "UISupportedInterfaceOrientations": [
            "UIInterfaceOrientationPortrait",
            "UIInterfaceOrientationLandscapeLeft",
            "UIInterfaceOrientationLandscapeRight",
        ],
        "UISupportedInterfaceOrientations~ipad": [
            "UIInterfaceOrientationPortrait",
            "UIInterfaceOrientationPortraitUpsideDown",
            "UIInterfaceOrientationLandscapeLeft",
            "UIInterfaceOrientationLandscapeRight",
        ],
    }


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def zip_tree(source: Path, destination: Path, prefix: str = "", *, compression: int = zipfile.ZIP_DEFLATED) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(destination, "w", compression=compression, compresslevel=9) as archive:
        for path in sorted(p for p in source.rglob("*") if p.is_file()):
            relative = path.relative_to(source).as_posix()
            name = f"{prefix.rstrip('/')}/{relative}" if prefix else relative
            info = zipfile.ZipInfo(name, date_time=(2000, 1, 1, 0, 0, 0))
            mode = 0o755 if os.access(path, os.X_OK) else 0o644
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = compression
            archive.writestr(info, path.read_bytes())


def write_checksum(path: Path) -> None:
    path.with_name(path.name + ".sha256").write_text(f"{sha256(path)}  {path.name}\n", encoding="ascii")


def validate_app_contents(app: Path, *, allow_signature: bool) -> None:
    expected = {APP_NAME, "Info.plist", "HorizonBuildInfo.json", "Licenses", *ICON_SPECS}
    actual = {path.name for path in app.iterdir()}
    missing = expected - actual
    if missing:
        raise ValueError(f"incomplete app bundle; missing: {sorted(missing)}")
    allowed = expected | ({"_CodeSignature"} if allow_signature else set())
    if actual - allowed:
        raise ValueError(f"unexpected app bundle contents: {sorted(actual - allowed)}")
    licenses = app / "Licenses"
    if not licenses.is_dir() or {path.name for path in licenses.iterdir()} != set(LICENSE_SOURCES):
        raise ValueError("incomplete or unexpected bundled license notices")
    if any(not path.is_file() or path.is_symlink() for path in licenses.iterdir()):
        raise ValueError("invalid bundled license notice")
    if "_CodeSignature" in actual and not (app / "_CodeSignature").is_dir():
        raise ValueError("invalid app signature directory")
    if (app / "embedded.mobileprovision").exists():
        raise ValueError("package must not embed a provisioning profile")


def package(args: argparse.Namespace) -> Path:
    binary = args.binary.resolve()
    icon = args.icon.resolve()
    output_dir = args.output_dir.resolve()
    if not binary.is_file():
        raise ValueError(f"client executable does not exist: {binary}")
    if not icon.is_file():
        raise ValueError(f"project icon does not exist: {icon}")
    validate_arm64_macho(binary)
    if args.minimum_os != MINIMUM_IOS:
        raise ValueError(f"this package is configured for minimum iOS {MINIMUM_IOS}")
    if not args.version or any(not part.isdigit() for part in args.version.split(".")):
        raise ValueError("version must be numeric, for example 0.2.48")
    if not args.build_number.isdigit():
        raise ValueError("build number must be numeric")

    output_dir.mkdir(parents=True, exist_ok=True)
    app = output_dir / f"{APP_NAME}.app"
    if app.exists():
        shutil.rmtree(app)
    app.mkdir()
    executable = app / APP_NAME
    shutil.copyfile(binary, executable)
    executable.chmod(0o755)
    icon_hashes = make_icons(icon, app)
    licenses = app / "Licenses"
    licenses.mkdir()
    license_hashes = {}
    for filename, source in LICENSE_SOURCES.items():
        destination = licenses / filename
        shutil.copyfile(source, destination)
        license_hashes[filename] = sha256(destination)
    (app / "Info.plist").write_bytes(
        plistlib.dumps(app_info(args.target, args.version, args.build_number, args.sdk_version), fmt=plistlib.FMT_XML, sort_keys=True)
    )

    dsym_hash = None
    if args.dsym:
        dsym = args.dsym.resolve()
        if not dsym.is_dir():
            raise ValueError(f"dSYM bundle does not exist: {dsym}")
        dwarf_files = list((dsym / "Contents/Resources/DWARF").glob("*"))
        if len(dwarf_files) != 1 or not dwarf_files[0].is_file():
            raise ValueError(f"expected one DWARF file in {dsym}")
        dsym_hash = tree_sha256(dsym)

    metadata = {
        "schema": 1,
        "application": APP_NAME,
        "bundle_id": BUNDLE_ID,
        "version": args.version,
        "build_number": args.build_number,
        "platform": args.target,
        "target": "aarch64-apple-ios" if args.target == "device" else "aarch64-apple-ios-sim",
        "minimum_os": MINIMUM_IOS,
        "features": ["audio", "ios"],
        "profile": "release (line tables only)",
        "commit": args.commit,
        "toolchain": {
            "rustc": args.rustc_version,
            "cargo": args.cargo_version,
            "xcode": args.xcode_version,
            "sdk": args.sdk_version,
        },
        "sha256": {
            "rust_macho": sha256(binary),
            "project_icon_source": sha256(icon),
            "icons": icon_hashes,
            "license_notices": license_hashes,
            "dsym_tree": dsym_hash,
        },
    }
    write_json(app / "HorizonBuildInfo.json", metadata)
    metadata_path = output_dir / "build-metadata.json"
    write_json(metadata_path, metadata)
    write_checksum(metadata_path)

    if args.target == "simulator" and args.adhoc_sign:
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(app)], check=True)
    elif args.target == "device" and args.adhoc_sign:
        raise ValueError("device IPA packages must remain unsigned and re-signable")
    validate_app_contents(app, allow_signature=args.target == "simulator" and args.adhoc_sign)

    artifact = output_dir / (f"{APP_NAME}.ipa" if args.target == "device" else f"{APP_NAME}-simulator.app.zip")
    if args.target == "device":
        # Stage Payload in a private temporary directory. The output folder may
        # already contain unrelated user data named "Payload".
        with tempfile.TemporaryDirectory(prefix=".horizon-ios-payload-", dir=output_dir) as staging:
            payload = Path(staging) / "Payload"
            payload.mkdir()
            shutil.copytree(app, payload / app.name)
            if any(p.name in {"_CodeSignature", "embedded.mobileprovision"} for p in (payload / app.name).rglob("*")):
                raise ValueError("device IPA must not contain signing state or a provisioning profile")
            zip_tree(payload, artifact, "Payload")
    else:
        zip_tree(app, artifact, app.name)
    write_checksum(artifact)
    return artifact


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--target", choices=("device", "simulator"), required=True)
    result.add_argument("--binary", type=Path, required=True, help="Cargo-built arm64 horizon-client Mach-O")
    result.add_argument("--icon", type=Path, default=ICON_SOURCE, help="original Horizon PNG icon")
    result.add_argument("--output-dir", type=Path, required=True)
    result.add_argument("--commit", required=True)
    result.add_argument("--version", default="0.2.48")
    result.add_argument("--build-number", default=os.environ.get("GITHUB_RUN_NUMBER", "1"))
    result.add_argument("--minimum-os", default=MINIMUM_IOS)
    result.add_argument("--sdk-version", required=True)
    result.add_argument("--rustc-version", required=True)
    result.add_argument("--cargo-version", required=True)
    result.add_argument("--xcode-version", required=True)
    result.add_argument("--dsym", type=Path)
    result.add_argument("--adhoc-sign", action="store_true", help="sign the simulator app with the local ad-hoc identity")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        artifact = package(args)
    except (OSError, ValueError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        print(f"package_ios: {error}", file=sys.stderr)
        return 2
    print(f"created {artifact}")
    print(f"sha256 {sha256(artifact)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
