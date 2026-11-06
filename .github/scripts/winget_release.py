#!/usr/bin/env python3
"""Submit verified, already-published Rust Switcher EXEs to the WinGet fork."""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request

OWNER = "qqrm"
REPO = "rust-switcher-rs"
WINGET_FORK = "qqrm/winget-pkgs"
WINGET_UPSTREAM = "microsoft/winget-pkgs"
PACKAGE_ID = "QQRM.RustSwitcher"
MANIFEST_VERSION = "1.28.0"
MANIFEST_ROOT = "manifests/q/QQRM/RustSwitcher"
VERSION_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


def api(url: str) -> object:
    headers = {"Accept": "application/vnd.github+json", "User-Agent": "QQRM-RustSwitcher-WinGet"}
    token = os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(req, timeout=60) as response:
        return json.load(response)


def semver(value: str) -> tuple[int, int, int]:
    match = VERSION_RE.fullmatch(value)
    if not match:
        raise ValueError(f"Not a stable numeric release tag: {value!r}")
    return tuple(map(int, match.groups()))


def download_asset(url: str, destination: Path, expected_size: int) -> str:
    parsed = urllib.parse.urlparse(url)
    expected_path = f"/{OWNER}/{REPO}/releases/download/{os.environ['RELEASE_TAG']}/rust-switcher.exe"
    if parsed.scheme != "https" or parsed.hostname != "github.com" or parsed.path != expected_path:
        raise ValueError("Unexpected executable asset URL")
    request = urllib.request.Request(url, headers={"User-Agent": "QQRM-RustSwitcher-WinGet"})
    digest = hashlib.sha256()
    total = 0
    with urllib.request.urlopen(request, timeout=120) as response, destination.open("wb") as output:
        final = urllib.parse.urlparse(response.geturl())
        if final.scheme != "https" or final.hostname not in {"github.com", "release-assets.githubusercontent.com"}:
            raise ValueError("Executable download left the GitHub release asset hosts")
        while block := response.read(1024 * 1024):
            total += len(block)
            if total > expected_size:
                raise ValueError("Executable is larger than the GitHub release metadata")
            digest.update(block)
            output.write(block)
    if total != expected_size or total < 4096:
        raise ValueError("Executable size does not match the uploaded release asset")
    return digest.hexdigest().upper()


def verify_pe_x64(path: Path) -> None:
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) < 64 or header[:2] != b"MZ":
            raise ValueError("Release asset is not a Windows PE executable")
        pe_offset = int.from_bytes(header[0x3C:0x40], "little")
        if pe_offset < 64 or pe_offset > 16 * 1024 * 1024:
            raise ValueError("Invalid PE header offset")
        stream.seek(pe_offset)
        pe = stream.read(6)
        if len(pe) != 6 or pe[:4] != b"PE\0\0" or int.from_bytes(pe[4:6], "little") != 0x8664:
            raise ValueError("Release asset is not an x64 Windows PE executable")


def release_for_event() -> tuple[dict, str, str]:
    requested = os.environ.get("RELEASE_TAG", "").strip()
    if requested:
        tag = requested if requested.startswith("v") else "v" + requested
        semver(tag)
        release = api(f"https://api.github.com/repos/{OWNER}/{REPO}/releases/tags/{tag}")
    else:
        release = api(f"https://api.github.com/repos/{OWNER}/{REPO}/releases/latest")
        tag = release.get("tag_name", "")
    version = ".".join(map(str, semver(tag)))
    if release.get("tag_name") != tag:
        raise ValueError("Release metadata does not match the requested tag")
    if release.get("draft") or release.get("prerelease") or not release.get("published_at"):
        raise ValueError("Only a published stable release can be submitted")
    latest = api(f"https://api.github.com/repos/{OWNER}/{REPO}/releases/latest")
    if latest.get("id") != release.get("id") or latest.get("tag_name") != tag:
        raise ValueError("Requested release is stale; it is not the latest stable release")
    os.environ["RELEASE_TAG"] = tag
    return release, tag, version


def verify_executable(release: dict, tag: str) -> str:
    asset_name = "rust-switcher.exe"
    matches = [asset for asset in release.get("assets", []) if asset.get("name") == asset_name]
    if len(matches) != 1:
        raise ValueError("Release must contain exactly one rust-switcher.exe")
    asset = matches[0]
    if asset.get("state") != "uploaded" or asset.get("content_type") != "application/x-msdownload":
        raise ValueError("Windows executable is not a completed EXE release asset")
    advertised = asset.get("digest", "")
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", advertised):
        raise ValueError("GitHub did not publish a SHA-256 digest for the executable")
    os.environ["RELEASE_TAG"] = tag
    path = Path("rust-switcher.exe")
    actual = download_asset(asset["browser_download_url"], path, int(asset["size"]))
    verify_pe_x64(path)
    if actual.lower() != advertised.removeprefix("sha256:").lower():
        raise ValueError("Downloaded executable SHA-256 differs from GitHub release metadata")
    return actual


def current_versions() -> list[tuple[int, int, int]]:
    entries = api(f"https://api.github.com/repos/{WINGET_UPSTREAM}/contents/{MANIFEST_ROOT}?ref=master")
    found = []
    for entry in entries:
        if entry.get("type") == "dir":
            try:
                found.append(semver("v" + entry["name"]))
            except ValueError:
                continue
    return found


def run_gh(*args: str) -> str:
    result = subprocess.run(["gh", *args], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f"gh {' '.join(args[:3])} failed: {result.stdout.strip()}")
    return result.stdout.strip()


def manifests(version: str, published_at: str, sha256: str) -> dict[str, str]:
    semver("v" + version)
    if not re.fullmatch(r"[0-9a-fA-F]{64}", sha256):
        raise ValueError("Installer SHA-256 must contain exactly 64 hexadecimal characters")
    sha256 = sha256.upper()
    date = dt.datetime.fromisoformat(published_at.replace("Z", "+00:00")).date().isoformat()
    return {
        f"{MANIFEST_ROOT}/{version}/{PACKAGE_ID}.yaml": f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.{MANIFEST_VERSION}.schema.json
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: {MANIFEST_VERSION}
""",
        f"{MANIFEST_ROOT}/{version}/{PACKAGE_ID}.installer.yaml": f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.{MANIFEST_VERSION}.schema.json
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
MinimumOSVersion: 10.0.22000.0
InstallerType: portable
Commands:
- rust-switcher
Dependencies:
  PackageDependencies:
  - PackageIdentifier: Microsoft.VCRedist.2015+.x64
ReleaseDate: {date}
Installers:
- Architecture: x64
  InstallerUrl: https://github.com/{OWNER}/{REPO}/releases/download/v{version}/rust-switcher.exe
  InstallerSha256: {sha256}
ManifestType: installer
ManifestVersion: {MANIFEST_VERSION}
""",
        f"{MANIFEST_ROOT}/{version}/{PACKAGE_ID}.locale.en-US.yaml": f"""# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.{MANIFEST_VERSION}.schema.json
PackageIdentifier: {PACKAGE_ID}
PackageVersion: {version}
PackageLocale: en-US
Publisher: QQRM
PublisherUrl: https://github.com/qqrm
PublisherSupportUrl: https://github.com/qqrm/rust-switcher-rs/issues
PrivacyUrl: https://github.com/qqrm/rust-switcher-rs/blob/dev/PRIVACY.md
PackageName: Rust Switcher
PackageUrl: https://github.com/qqrm/rust-switcher-rs
License: MIT
LicenseUrl: https://github.com/qqrm/rust-switcher-rs/blob/v{version}/LICENSE
ShortDescription: Windows 11 keyboard-layout switcher for converting Russian and English text.
Description: Rust Switcher is a native Windows 11 utility that converts selected text and recently typed words between Russian and English keyboard layouts. It provides configurable hotkeys, optional automatic conversion, a tray icon, settings, and autostart.
Moniker: rust-switcher
Tags:
- keyboard-layout
- productivity
- russian
- tray
- windows
ReleaseNotesUrl: https://github.com/qqrm/rust-switcher-rs/releases/tag/v{version}
ManifestType: defaultLocale
ManifestVersion: {MANIFEST_VERSION}
""",
    }


def open_prs() -> list[dict]:
    query = urllib.parse.quote(f'repo:{WINGET_UPSTREAM} is:pr "{PACKAGE_ID}"')
    data = api(f"https://api.github.com/search/issues?q={query}&per_page=100")
    items = data.get("items")
    if (not isinstance(items, list) or data.get("incomplete_results") is not False
            or data.get("total_count") != len(items)):
        raise ValueError("Incomplete or truncated WinGet PR search; refusing a possible duplicate")
    return [item for item in items if item.get("pull_request")]


def manifest_bytes(content: str) -> bytes:
    """Use identical UTF-8/CRLF bytes for validation and the Contents API."""
    return content.replace("\r\n", "\n").replace("\n", "\r\n").encode("utf-8")


def validate_manifests(files: dict[str, str], version: str) -> None:
    """Run the official WinGet validator on the exact files that will be submitted."""
    with tempfile.TemporaryDirectory(prefix="rust-switcher-winget-") as temporary:
        root = Path(temporary)
        for relative, content in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(manifest_bytes(content))
        manifest_dir = root / MANIFEST_ROOT / version
        result = subprocess.run(["winget", "validate", "--manifest", str(manifest_dir),
                                 "--disable-interactivity"],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        if result.returncode:
            raise RuntimeError(f"winget validate failed; no PR was created: {result.stdout.strip()}")
        print(result.stdout.strip())


def publication_needed(version: str) -> bool:
    """Fail closed on competing submissions, including closed/rejected attempts."""
    wanted = semver("v" + version)
    published = current_versions()
    if published and max(published) >= wanted:
        print(f"WinGet already contains {max(published)} or newer; no action")
        return False
    existing = open_prs()
    version_title = re.compile(rf"\b{re.escape(version)}\b")
    exact = [item for item in existing if version_title.search(item.get("title", ""))]
    open_exact = [item for item in exact if item.get("state") == "open"]
    if len(open_exact) > 1:
        raise ValueError("Multiple open WinGet PRs exist for this package/version")
    if open_exact:
        print(f"WinGet PR already exists for {version}: {open_exact[0]['html_url']}")
        return False
    if exact:
        raise ValueError(f"A closed PR already exists for {version}; refusing to duplicate it: {exact[0]['html_url']}")
    open_package = [item for item in existing if item.get("state") == "open"]
    if open_package:
        raise ValueError("Another QQRM.RustSwitcher PR is open; refusing overlapping manifest updates")
    return True


def submit(version: str, release: dict, sha256: str) -> None:
    if not publication_needed(version):
        return

    branch = f"manifest/qqrm-rustswitcher-{version}"
    files = manifests(version, release["published_at"], sha256)
    validate_manifests(files, version)
    # Reject releases that became stale while their files were being validated.
    latest = api(f"https://api.github.com/repos/{OWNER}/{REPO}/releases/latest")
    if latest.get("id") != release.get("id") or latest.get("tag_name") != "v" + version:
        raise ValueError("Release became stale before PR creation")
    current = run_gh("api", f"repos/{WINGET_UPSTREAM}/git/ref/heads/master", "--jq", ".object.sha")
    ref_result = subprocess.run(["gh", "api", "--method", "POST", f"repos/{WINGET_FORK}/git/refs",
                                 "-f", f"ref=refs/heads/{branch}", "-f", f"sha={current}"],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if ref_result.returncode:
        if "Reference already exists" not in ref_result.stdout:
            raise RuntimeError(f"Could not create WinGet branch: {ref_result.stdout.strip()}")
        raise ValueError(f"Branch {branch} already exists without a matching open PR; inspect it manually")
    for path, content in files.items():
        encoded = base64.b64encode(manifest_bytes(content)).decode("ascii")
        run_gh("api", "--method", "PUT", f"repos/{WINGET_FORK}/contents/{path}",
               "-f", f"message=New version: {PACKAGE_ID} version {version}",
               "-f", f"content={encoded}", "-f", f"branch={branch}")
    # Other authors can submit or publish while these API writes are in flight.
    latest = api(f"https://api.github.com/repos/{OWNER}/{REPO}/releases/latest")
    if latest.get("id") != release.get("id") or latest.get("tag_name") != "v" + version:
        raise ValueError("Release became stale before PR creation")
    if not publication_needed(version):
        return
    body = (f"## 📖 Description\n\nUpdates `{PACKAGE_ID}` to version `{version}` using the published Windows x64 release executable.\n\n"
            f"The executable was downloaded from the version-specific release URL, checked as an x64 Windows PE, and its SHA-256 was verified against GitHub release asset metadata: `{sha256}`.\n\n"
            "## 📦 Manifest Checklist\n\n- [x] One manifest set for this package/version\n"
            "- [x] Installer URL and SHA-256 verified from the published release asset\n"
            f"- [x] Manifest schema version `{MANIFEST_VERSION}`\n- [x] Validated on Windows with `winget validate --manifest`\n"
            "- [ ] Tested locally with `winget install --manifest`\n")
    pr = subprocess.run(["gh", "api", "--method", "POST", f"repos/{WINGET_UPSTREAM}/pulls",
                         "-f", f"title=New version: {PACKAGE_ID} version {version}",
                         "-f", "base=master", "-f", f"head=qqrm:{branch}", "-f", f"body={body}"],
                        text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if pr.returncode:
        raise RuntimeError(f"Could not open Microsoft WinGet PR: {pr.stdout.strip()}")
    result = json.loads(pr.stdout)
    print(f"Created WinGet PR {result['html_url']}")


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--validate-only", action="store_true",
                        help="Verify the release asset and run WinGet validation without GitHub writes")
    args = parser.parse_args(argv)
    if not args.validate_only and not os.environ.get("GH_TOKEN"):
        raise ValueError("GH_TOKEN is required (configure WINGET_TOKEN with fork write and upstream PR rights)")
    release, tag, version = release_for_event()
    sha256 = verify_executable(release, tag)
    if args.validate_only:
        validate_manifests(manifests(version, release["published_at"], sha256), version)
        print(f"Validated {PACKAGE_ID} {version}; no GitHub writes requested")
        return
    submit(version, release, sha256)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"WinGet submission stopped: {error}", file=sys.stderr)
        raise
