#!/usr/bin/env python3
"""Stage a local Damiao SDK and print a Tauri config for a self-contained build.

Usage: python3 scripts/prepare-can-sdk.py /path/to/dm-device-sdk-master
The generated .can-sdk.local directory is ignored by git. This does not publish
or grant redistribution rights for the vendor libraries.
"""
import argparse
import json
import platform
import shutil
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("sdk", type=Path)
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
stage = root / ".can-sdk.local"
stage.mkdir(exist_ok=True)
base = args.sdk.resolve() / "C&C++" / "lib" / "v1.1.0"
arch = "arm64" if platform.machine().lower() in ("arm64", "aarch64") else "x86_64"
system = platform.system()
if system == "Darwin":
    sources = [base / "macos" / arch / "libdm_device.dylib"]
elif system == "Linux":
    sources = [base / "linux" / arch / "libdm_device.so"]
elif system == "Windows" and arch == "x86_64":
    sources = [base / "windows" / "msvc" / "dm_device.dll", args.sdk.resolve() / "CSharp/demo/bin/Debug/net8.0/libusb-1.0.dll"]
else:
    parser.error(f"No bundled SDK for {system}/{arch}")
resources = {}
for source in sources:
    if not source.is_file():
        parser.error(f"SDK library missing: {source}")
    dest = stage / source.name
    shutil.copy2(source, dest)
    resources[str(dest)] = dest.name
config = stage / "tauri.json"
config.write_text(json.dumps({"bundle": {"resources": resources}}, indent=2) + "\n")
print(config)
