"""Validate release metadata and assemble binstall archives; no publication side effects."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

PACKAGES = ["tuning-studio-api", "tuning-studio-trace", "tuning-studio-dwarf",
            "tuning-studio-carriers", "tuning-studio-core", "tuning-studio-app",
            "tuning-studio-server", "tuning-studio"]

def metadata():
    return json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"]))

def validate(version):
    data = metadata()
    packages = {p["name"]: p for p in data["packages"]}
    for name in PACKAGES:
        p = packages[name]
        if p["version"] != version:
            raise SystemExit(f"{name} is {p['version']}, expected {version}")
        if not p["license"] or not p["repository"] or not p["description"]:
            raise SystemExit(f"{name}: missing publishing metadata")
        for dep in p["dependencies"]:
            if dep.get("path") and dep["kind"] != "dev" and dep["req"] == "*":
                raise SystemExit(f"{name}: path dependency without a registry version")
    config = json.loads(Path("src-tauri/tauri.conf.json").read_text())
    if config["version"] != version:
        raise SystemExit("Tauri version differs from crate version")
    if not Path("src-tauri/frontend/index.html").is_file():
        raise SystemExit("Run npm ci && npm run build before packaging")
    files = subprocess.check_output(["cargo", "package", "-p", "tuning-studio", "--list", "--allow-dirty"], text=True).splitlines()
    if "frontend/index.html" not in files or not any(x.startswith("frontend/assets/") for x in files):
        raise SystemExit("Desktop crate is missing embedded frontend assets")
    print(f"Validated {len(PACKAGES)} packages at {version}")

def archive(version, target):
    suffix = ".exe" if "windows" in target else ""
    binary = Path("target") / target / "release" / f"tuning-studio{suffix}"
    if not binary.is_file():
        raise SystemExit(f"Missing {binary}")
    out = Path("release-assets")
    out.mkdir(exist_ok=True)
    dest = out / f"tuning-studio-{version}-{target}.tar.gz"
    with tarfile.open(dest, "w:gz") as tar:
        tar.add(binary, arcname=binary.name)
        tar.add("LICENSE", arcname="LICENSE")
    with tarfile.open(dest) as tar:
        assert f"tuning-studio{suffix}" in tar.getnames()
    digest = hashlib.sha256(dest.read_bytes()).hexdigest()
    dest.with_name(dest.name + ".sha256").write_text(f"{digest}  {dest.name}\n")
    print(dest)

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["validate", "archive"])
    parser.add_argument("--version", required=True)
    parser.add_argument("--target")
    args = parser.parse_args()
    if args.command == "validate":
        validate(args.version)
    elif args.target:
        archive(args.version, args.target)
    else:
        parser.error("archive requires --target")
