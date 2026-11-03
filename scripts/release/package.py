"""Prepare and verify the Android release artifact consumed by Actions."""

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from verify_android_bundle import verify

SIGNER_SHA256 = "b6da01480eefd5fbf2cd3771b8d1021ec791304bdd6c4bf41d3faabad48ee5e1"
VERSION_PATTERN = r"(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"


def release_identity(version, code, tag, mode):
    if mode not in ("preview", "release"):
        raise ValueError("Unknown release mode")
    if not re.fullmatch(VERSION_PATTERN, version) or tag != f"v{version}":
        raise ValueError("Release tag must equal v<Gradle versionName>")
    if not 1 <= int(code) <= 2100000000:
        raise ValueError("Invalid Android versionCode")
    preview = mode == "preview"
    return {"version": version, "versionCode": int(code), "tag": tag, "mode": mode,
            "applicationId": "com.coomidev.android" if preview else "com.coomi.android",
            "channel": "preview" if preview else ("test" if re.search(r"-(?:test|preview|alpha|beta|rc)(?:[.-]|$)", version) else "stable"),
            "file": f"{'CoomiDev' if preview else 'Coomi'}-Android-arm64-{tag}.apk"}


def source_identity(tag, mode):
    gradle = (ROOT / "apps/coomi-app/app/build.gradle").read_text(encoding="utf-8")
    version = re.search(r'^\s*versionName "([^"]+)"$', gradle, re.MULTILINE).group(1)
    code = re.search(r'^\s*versionCode (\d+)$', gradle, re.MULTILINE).group(1)
    identity = release_identity(version, code, tag or f"v{version}", mode)
    notes = ROOT / f"docs/releases/{identity['tag']}.md"
    if not notes.is_file() or not notes.read_text(encoding="utf-8").strip():
        raise ValueError(f"Missing release notes: {notes}")
    return identity


def verify_identity(apk, metadata, sdk):
    tools = sdk / ("build-tools/" + os.environ.get("COOMI_BUILD_TOOLS", "36.0.0"))
    signer_tool = "apksigner.bat" if os.name == "nt" else "apksigner"
    aapt_tool = "aapt.exe" if os.name == "nt" else "aapt"
    signature = subprocess.check_output(
        [str(tools / signer_tool), "verify", "--verbose", "--print-certs", str(apk)], text=True)
    signer = re.search(r"Signer #1 certificate SHA-256 digest: ([0-9a-fA-F]+)", signature)
    if signer is None:
        raise ValueError("APK has no signing certificate")
    if metadata["mode"] == "release" and signer.group(1).lower() != SIGNER_SHA256:
        raise ValueError("APK certificate does not match the official Coomi signer")
    badging = subprocess.check_output([str(tools / aapt_tool), "dump", "badging", str(apk)], text=True)
    package = re.search(r"^package: name='([^']+)' versionCode='([^']+)' versionName='([^']+)'",
                        badging, re.MULTILINE)
    if package is None or package.groups() != (
            metadata["applicationId"], str(metadata["versionCode"]), metadata["version"]):
        raise ValueError("APK package/version differs from the source release")
    if re.search(r"^native-code: 'arm64-v8a'\s*$", badging, re.MULTILINE) is None:
        raise ValueError("APK must contain only ARM64 native code")


def package_release(identity, sdk, output):
    apk_dir = ROOT / "apps/coomi-app/app/build/outputs/apk/release"
    build = json.loads((apk_dir / "output-metadata.json").read_text(encoding="utf-8"))
    if build["applicationId"] != identity["applicationId"] or len(build["elements"]) != 1:
        raise ValueError("Expected one Coomi release APK")
    apk = apk_dir / build["elements"][0]["outputFile"]
    verify_identity(apk, identity, sdk)
    verify(apk)
    output.mkdir(parents=True, exist_ok=True)
    target = output / identity["file"]
    shutil.copyfile(apk, target)
    with target.open("rb") as content:
        digest = hashlib.file_digest(content, "sha256").hexdigest()
    notes_path = ROOT / f"docs/releases/{identity['tag']}.md"
    notes = notes_path.read_text(encoding="utf-8").strip()
    now = datetime.now(timezone.utc).replace(microsecond=0)
    manifest = {**identity, "platform": "android", "arch": "arm64-v8a", "minAndroid": "7.0",
                "date": now.astimezone(timezone(timedelta(hours=8))).date().isoformat(),
                "publishedAt": now.isoformat().replace("+00:00", "Z"),
                "size": target.stat().st_size, "sha256": digest, "notes": notes,
                "url": f"https://gh-proxy.com/https://github.com/{os.environ.get('GITHUB_REPOSITORY', 'TensorHub-ORG/Coomi')}/releases/download/{identity['tag']}/{identity['file']}",
                "sourceCommit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()}
    heading = re.search(r"^#\s+(.+更新说明【(?:稳定|测试)】)\s*$", notes, re.MULTILINE)
    if heading:
        manifest["websiteHeading"] = heading.group(1)
    (output / "latest.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (output / f"{target.name}.sha256").write_text(f"{digest}  {target.name}\n", encoding="utf-8")
    shutil.copyfile(notes_path, output / "release-notes.md")
    shutil.copyfile(ROOT / "runtime-v2-dist/runtime-v2-manifest.json", output / "runtime-v2-manifest.json")
    print(json.dumps({"file": target.name, "sha256": digest, "versionCode": identity["versionCode"]}))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=["metadata", "package"])
    parser.add_argument("--tag", default="")
    parser.add_argument("--mode", choices=["preview", "release"], default="preview")
    parser.add_argument("--output", type=Path, default=ROOT / "release-dist")
    args = parser.parse_args()
    identity = source_identity(args.tag, args.mode)
    if args.command == "metadata":
        print(json.dumps(identity))
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            for key in ("tag", "version", "channel", "mode"):
                output.write(f"{key}={identity[key]}\n")
    else:
        package_release(identity, Path(os.environ["ANDROID_HOME"]), args.output)


if __name__ == "__main__":
    main()
