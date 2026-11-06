"""Publication regression tests; every network request and child process is stubbed."""
from __future__ import annotations

import base64
import contextlib
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import urllib.parse

import winget_release as winget


VERSION = "1.0.26"
TAG = "v" + VERSION
PUBLISHED = "2026-11-05T07:33:12Z"
UPSTREAM_SHA = "b" * 40
ASSET_URL = f"https://github.com/qqrm/rust-switcher-rs/releases/download/{TAG}/rust-switcher.exe"
PR_URL = "https://github.com/microsoft/winget-pkgs/pull/999999"


def executable() -> bytes:
    data = bytearray(4096)
    data[:2] = b"MZ"
    data[0x3C:0x40] = (64).to_bytes(4, "little")
    data[64:70] = b"PE\0\0\x64\x86"
    return bytes(data)


SHA256 = hashlib.sha256(executable()).hexdigest().upper()


def release() -> dict:
    return {
        "id": 26, "tag_name": TAG, "draft": False, "prerelease": False,
        "published_at": PUBLISHED,
        "assets": [{
            "name": "rust-switcher.exe", "state": "uploaded",
            "content_type": "application/x-msdownload", "size": len(executable()),
            "digest": "sha256:" + SHA256.lower(), "browser_download_url": ASSET_URL,
        }],
    }


def pull_request(version: str = VERSION, state: str = "open") -> dict:
    return {
        "title": f"New version: QQRM.RustSwitcher version {version}",
        "state": state, "html_url": PR_URL, "pull_request": {"url": PR_URL},
    }


class Response(io.BytesIO):
    def __init__(self, content: bytes, url: str = ASSET_URL):
        super().__init__(content)
        self.url = url

    def geturl(self) -> str:
        return self.url


class OfflineTestCase(unittest.TestCase):
    """Tests fail immediately if code tries unapproved network or process access."""

    def setUp(self):
        self.enterContext(patch.dict(os.environ, {
            "GH_TOKEN": "unit-test-only", "RELEASE_TAG": TAG,
        }, clear=True))
        temporary = self.enterContext(tempfile.TemporaryDirectory())
        self.enterContext(contextlib.chdir(temporary))
        self.output = self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.network = self.enterContext(patch.object(
            winget.urllib.request, "urlopen",
            side_effect=AssertionError("Unexpected network request in publication unit test")))
        self.process = self.enterContext(patch.object(
            winget.subprocess, "run",
            side_effect=AssertionError("Unexpected child process in publication unit test")))
        self.enterContext(patch.object(
            socket, "create_connection", side_effect=AssertionError("Network access is forbidden")))
        self.enterContext(patch.object(
            subprocess, "Popen", side_effect=AssertionError("Real child processes are forbidden")))


class PublicationSandbox:
    """An allowlist for reads, validator bytes, fork writes, and one upstream PR."""

    def __init__(self, case: unittest.TestCase, files: dict[str, str] | None = None):
        self.case = case
        self.latest = release()
        self.versions = [(1, 0, 20)]
        self.prs = []
        self.search_complete = True
        self.search_count = None
        self.files = files if files is not None else winget.manifests(VERSION, PUBLISHED, SHA256)
        self.validated = {}
        self.uploaded = {}
        self.mutations = []
        self.pr_fields = None
        self.validation_failure = False
        self.branch_failure = ""
        self.content_failure = 0
        self.pr_failure = False
        self.after_upload = None

    def read_api(self, url: str):
        parsed = urllib.parse.urlparse(url)
        self.case.assertEqual((parsed.scheme, parsed.netloc), ("https", "api.github.com"))
        if parsed.path in {
            "/repos/qqrm/rust-switcher-rs/releases/latest",
            f"/repos/qqrm/rust-switcher-rs/releases/tags/{TAG}",
        }:
            return copy.deepcopy(self.latest)
        if parsed.path == f"/repos/microsoft/winget-pkgs/contents/{winget.MANIFEST_ROOT}":
            self.case.assertEqual(urllib.parse.parse_qs(parsed.query), {"ref": ["master"]})
            return [{"type": "dir", "name": ".".join(map(str, version))} for version in self.versions]
        if parsed.path == "/search/issues":
            query = urllib.parse.parse_qs(parsed.query)
            self.case.assertIn("repo:microsoft/winget-pkgs", query["q"][0])
            self.case.assertIn("QQRM.RustSwitcher", query["q"][0])
            return {
                "items": copy.deepcopy(self.prs), "incomplete_results": not self.search_complete,
                "total_count": len(self.prs) if self.search_count is None else self.search_count,
            }
        self.case.fail(f"Unexpected read API URL: {url}")

    def run(self, args, **kwargs):
        self.case.assertNotIn("shell", kwargs)
        if args[:3] == ["winget", "validate", "--manifest"]:
            self.case.assertIn("--disable-interactivity", args)
            directory = Path(args[3])
            self.case.assertEqual(directory.name, VERSION)
            actual = {path.name: path.read_bytes() for path in directory.iterdir()}
            self.case.assertEqual(set(actual), {Path(path).name for path in self.files})
            for path, content in self.files.items():
                expected = content.replace("\r\n", "\n").replace("\n", "\r\n").encode("utf-8")
                self.case.assertEqual(actual[Path(path).name], expected)
            if self.validation_failure:
                return subprocess.CompletedProcess(args, 1, "Manifest validation failed")
            self.validated = {path: actual[Path(path).name] for path in self.files}
            return subprocess.CompletedProcess(args, 0, "Manifest validation succeeded")

        self.case.assertEqual(args[:2], ["gh", "api"])
        endpoints = [argument for argument in args if argument.startswith("repos/")]
        self.case.assertEqual(len(endpoints), 1)
        endpoint = endpoints[0]
        method = args[args.index("--method") + 1] if "--method" in args else "GET"
        fields = {}
        for index, argument in enumerate(args):
            if argument == "-f":
                key, value = args[index + 1].split("=", 1)
                self.case.assertNotIn(key, fields)
                fields[key] = value
        if method == "GET" and endpoint == "repos/microsoft/winget-pkgs/git/ref/heads/master":
            self.case.assertEqual(args[-2:], ["--jq", ".object.sha"])
            return subprocess.CompletedProcess(args, 0, UPSTREAM_SHA)

        self.case.assertEqual(set(self.validated), set(self.files), "No write may precede validation")
        self.mutations.append((method, endpoint))
        branch = f"manifest/qqrm-rustswitcher-{VERSION}"
        title = f"New version: QQRM.RustSwitcher version {VERSION}"
        if method == "POST" and endpoint == "repos/qqrm/winget-pkgs/git/refs":
            self.case.assertEqual(fields, {"ref": f"refs/heads/{branch}", "sha": UPSTREAM_SHA})
            if self.branch_failure:
                return subprocess.CompletedProcess(args, 1, self.branch_failure)
            return subprocess.CompletedProcess(args, 0, "{}")
        prefix = "repos/qqrm/winget-pkgs/contents/"
        if method == "PUT" and endpoint.startswith(prefix):
            path = endpoint.removeprefix(prefix)
            self.case.assertIn(path, self.files)
            self.case.assertEqual(set(fields), {"message", "content", "branch"})
            self.case.assertEqual(fields["branch"], branch)
            self.case.assertEqual(fields["message"], title)
            decoded = base64.b64decode(fields["content"], validate=True)
            self.case.assertEqual(decoded, self.validated[path], "Upload must match validated UTF-8 bytes")
            self.case.assertNotIn(path, self.uploaded)
            if self.content_failure == len(self.uploaded) + 1:
                return subprocess.CompletedProcess(args, 1, "HTTP 403: Resource not accessible")
            self.uploaded[path] = decoded
            if len(self.uploaded) == len(self.files) and self.after_upload is not None:
                self.after_upload()
            return subprocess.CompletedProcess(args, 0, "{}")
        if method == "POST" and endpoint == "repos/microsoft/winget-pkgs/pulls":
            self.case.assertEqual(self.uploaded, self.validated)
            self.case.assertEqual(set(fields), {"title", "base", "head", "body"})
            self.case.assertEqual(fields["title"], title)
            self.case.assertEqual(fields["head"], f"qqrm:{branch}")
            self.case.assertEqual(fields["base"], "master")
            self.case.assertIn(SHA256, fields["body"])
            self.case.assertIn("winget validate", fields["body"])
            self.case.assertIsNone(self.pr_fields, "Never open a second PR in one submission")
            self.pr_fields = fields
            if self.pr_failure:
                return subprocess.CompletedProcess(args, 1, "HTTP 403: Pull requests denied")
            return subprocess.CompletedProcess(args, 0, json.dumps({"html_url": PR_URL}))
        self.case.fail(f"Unexpected mutation (including any upstream merge): {method} {endpoint}")


class ReleaseTests(OfflineTestCase):
    def test_stable_versions_are_numeric_and_ordered(self):
        self.assertEqual(winget.semver("v1.10.2"), (1, 10, 2))
        self.assertGreater(winget.semver("v1.10.0"), winget.semver("v1.9.9"))
        self.assertEqual(winget.semver("v0.0.0"), (0, 0, 0))

    def test_unstable_ambiguous_and_non_ascii_versions_are_rejected(self):
        for value in ("1.0.26", "v01.0.26", "v1.00.26", "v1.0.026", "v1.0.26-rc.1",
                      "v1.0.26+build", "v1.0.26\n", "v1.0", "v1.0.٢٦", "v１.0.26", "../v1.0.26"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                winget.semver(value)

    def test_requested_tag_and_bare_version_resolve_to_latest_stable(self):
        for requested in (TAG, VERSION, "  " + TAG + "  "):
            with self.subTest(requested=requested), patch.dict(os.environ, {"RELEASE_TAG": requested}), \
                    patch.object(winget, "api", return_value=release()) as api:
                selected, tag, version = winget.release_for_event()
                self.assertEqual((selected, tag, version), (release(), TAG, VERSION))
                self.assertEqual(os.environ["RELEASE_TAG"], TAG)
                self.assertTrue(api.call_args_list[0].args[0].endswith("/releases/tags/" + TAG))
                self.assertTrue(api.call_args_list[-1].args[0].endswith("/releases/latest"))

    def test_blank_request_selects_latest_release(self):
        with patch.dict(os.environ, {"RELEASE_TAG": ""}), \
                patch.object(winget, "api", return_value=release()) as api:
            self.assertEqual(winget.release_for_event()[1:], (TAG, VERSION))
            self.assertTrue(all(call.args[0].endswith("/releases/latest") for call in api.call_args_list))

    def test_draft_prerelease_unpublished_and_stale_releases_stop(self):
        for field, value in (("draft", True), ("prerelease", True), ("published_at", None)):
            invalid = release()
            invalid[field] = value
            with self.subTest(field=field), patch.object(winget, "api", return_value=invalid), \
                    self.assertRaises(ValueError):
                winget.release_for_event()
        latest = release()
        latest.update(id=27, tag_name="v1.0.27")
        with patch.object(winget, "api", side_effect=[release(), latest]), self.assertRaises(ValueError):
            winget.release_for_event()
        self.process.assert_not_called()

    def test_current_versions_ignore_files_and_non_version_directories(self):
        entries = [
            {"type": "dir", "name": "1.9.0"}, {"type": "dir", "name": "1.10.0"},
            {"type": "dir", "name": "1.11.0-rc.1"}, {"type": "dir", "name": "metadata"},
            {"type": "file", "name": "9.9.9"},
        ]
        with patch.object(winget, "api", return_value=entries):
            self.assertEqual(winget.current_versions(), [(1, 9, 0), (1, 10, 0)])


class ManifestTests(OfflineTestCase):
    def test_generated_manifest_set_has_consistent_identity_and_verified_installer(self):
        files = winget.manifests(VERSION, PUBLISHED, SHA256.lower())
        prefix = f"manifests/q/QQRM/RustSwitcher/{VERSION}/QQRM.RustSwitcher"
        self.assertEqual(set(files), {prefix + suffix for suffix in
                                     (".yaml", ".installer.yaml", ".locale.en-US.yaml")})
        for content in files.values():
            self.assertEqual(content.count("PackageIdentifier: QQRM.RustSwitcher\n"), 1)
            self.assertEqual(content.count(f"PackageVersion: {VERSION}\n"), 1)
            self.assertIn(f"ManifestVersion: {winget.MANIFEST_VERSION}\n", content)
            self.assertIn(f".{winget.MANIFEST_VERSION}.schema.json\n", content)
        self.assertIn("DefaultLocale: en-US\nManifestType: version\n", files[prefix + ".yaml"])
        installer = files[prefix + ".installer.yaml"]
        for line in ("MinimumOSVersion: 10.0.22000.0", "InstallerType: portable",
                     "Commands:\n- rust-switcher", "- Architecture: x64", "ReleaseDate: 2026-11-05",
                     f"InstallerUrl: {ASSET_URL}", f"InstallerSha256: {SHA256}",
                     "PackageIdentifier: Microsoft.VCRedist.2015+.x64"):
            self.assertIn(line, installer)
        locale = files[prefix + ".locale.en-US.yaml"]
        self.assertIn("PackageLocale: en-US\n", locale)
        self.assertIn("ManifestType: defaultLocale\n", locale)
        self.assertIn(f"LicenseUrl: https://github.com/qqrm/rust-switcher-rs/blob/{TAG}/LICENSE\n", locale)
        self.assertIn(f"ReleaseNotesUrl: https://github.com/qqrm/rust-switcher-rs/releases/tag/{TAG}\n", locale)

    def test_manifest_inputs_cannot_inject_paths_or_yaml(self):
        for version in ("../1.0.26", "1.0.26\nManifestType: other", "1.0.26-rc.1", "1.0.٢٦"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                winget.manifests(version, PUBLISHED, SHA256)
        for digest in ("", "a" * 63, "g" * 64, "a" * 64 + "\n", "a\n" * 32):
            with self.subTest(digest=digest), self.assertRaises(ValueError):
                winget.manifests(VERSION, PUBLISHED, digest)

    def test_manifest_bytes_are_utf8_crlf_and_idempotent(self):
        text = "Publisher: Пример\nDescription: café\r\n"
        expected = "Publisher: Пример\r\nDescription: café\r\n".encode("utf-8")
        self.assertEqual(winget.manifest_bytes(text), expected)
        self.assertEqual(winget.manifest_bytes(expected.decode("utf-8")), expected)


class AssetTests(OfflineTestCase):
    def test_real_hash_and_pe_verification_use_tokenless_asset_download(self):
        self.network.side_effect = lambda request, **kwargs: Response(executable())
        self.assertEqual(winget.verify_executable(release(), TAG), SHA256)
        request = self.network.call_args.args[0]
        self.assertEqual(request.full_url, ASSET_URL)
        self.assertIsNone(request.get_header("Authorization"))
        self.process.assert_not_called()

    def test_api_reads_use_only_the_explicit_test_token(self):
        self.network.return_value = Response(b'{"id": 26}')
        self.network.side_effect = None
        self.assertEqual(winget.api("https://api.github.com/repos/qqrm/rust-switcher-rs/releases/latest"), {"id": 26})
        request = self.network.call_args.args[0]
        self.assertEqual(request.get_header("Authorization"), "Bearer unit-test-only")

    def test_invalid_release_asset_metadata_stops_before_download(self):
        variants = []
        missing = release()
        missing["assets"] = []
        variants.append(missing)
        duplicate = release()
        duplicate["assets"] *= 2
        variants.append(duplicate)
        for field, value in (("state", "new"), ("content_type", "text/html"),
                             ("digest", ""), ("digest", "sha256:" + "g" * 64)):
            invalid = release()
            invalid["assets"][0][field] = value
            variants.append(invalid)
        for invalid in variants:
            with self.subTest(assets=invalid["assets"]), self.assertRaises(ValueError):
                winget.verify_executable(invalid, TAG)
        self.network.assert_not_called()
        self.process.assert_not_called()

    def test_digest_mismatch_stops_before_publication(self):
        invalid = release()
        invalid["assets"][0]["digest"] = "sha256:" + "f" * 64
        self.network.side_effect = lambda request, **kwargs: Response(executable())
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            winget.verify_executable(invalid, TAG)
        self.process.assert_not_called()

    def test_pe_rejects_non_executables_wrong_architecture_and_bad_offsets(self):
        variants = [b"MZ", b"<html>not an executable</html>", executable()[:69]]
        for offset, replacement in ((0, b"NO"), (0x3C, (63).to_bytes(4, "little")),
                                    (0x3C, (16 * 1024 * 1024 + 1).to_bytes(4, "little")),
                                    (0x3C, (8192).to_bytes(4, "little")), (64, b"NOPE"),
                                    (68, b"\x4c\x01"), (68, b"\x64\xaa")):
            invalid = bytearray(executable())
            invalid[offset:offset + len(replacement)] = replacement
            variants.append(bytes(invalid))
        path = Path("candidate.exe")
        path.write_bytes(executable())
        winget.verify_pe_x64(path)
        for index, invalid in enumerate(variants):
            with self.subTest(index=index):
                path.write_bytes(invalid)
                with self.assertRaises(ValueError):
                    winget.verify_pe_x64(path)

    def test_download_rejects_unexpected_origin_tag_and_redirect(self):
        for url in (ASSET_URL.replace("https:", "http:"), ASSET_URL.replace("github.com", "example.com"),
                    ASSET_URL.replace(TAG, "v1.0.25"), ASSET_URL + "/other.exe"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                winget.download_asset(url, Path("asset.exe"), len(executable()))
        self.network.assert_not_called()
        for final in ("https://example.com/asset", "http://github.com/asset"):
            self.network.side_effect = lambda request, url=final, **kwargs: Response(executable(), url)
            with self.subTest(final=final), self.assertRaises(ValueError):
                winget.download_asset(ASSET_URL, Path("asset.exe"), len(executable()))

    def test_download_checks_both_size_boundaries_and_minimum_size(self):
        for data, size in ((executable(), 4095), (executable(), 4097), (b"a" * 128, 128)):
            self.network.side_effect = lambda request, body=data, **kwargs: Response(body)
            with self.subTest(size=size), self.assertRaises(ValueError):
                winget.download_asset(ASSET_URL, Path("asset.exe"), size)

    def test_streamed_download_hashes_every_block(self):
        data = b"x" * (2 * 1024 * 1024 + 17)
        self.network.side_effect = lambda request, **kwargs: Response(data, "https://release-assets.githubusercontent.com/asset")
        path = Path("asset.exe")
        self.assertEqual(winget.download_asset(ASSET_URL, path, len(data)), hashlib.sha256(data).hexdigest().upper())
        self.assertEqual(path.read_bytes(), data)


class PublicationTests(OfflineTestCase):
    def submit(self, sandbox):
        self.process.side_effect = sandbox.run
        with patch.object(winget, "api", side_effect=sandbox.read_api):
            winget.submit(VERSION, release(), SHA256)

    def test_publication_uploads_base64_of_exact_validated_bytes_and_opens_one_pr(self):
        files = {path: text + "# Проверка UTF-8: café\n" for path, text in
                 winget.manifests(VERSION, PUBLISHED, SHA256).items()}
        sandbox = PublicationSandbox(self, files)
        with patch.object(winget, "manifests", return_value=files):
            self.submit(sandbox)
        self.assertEqual(len(sandbox.mutations), 5)
        self.assertEqual(sandbox.uploaded, sandbox.validated)
        self.assertEqual(sandbox.mutations[-1], ("POST", "repos/microsoft/winget-pkgs/pulls"))
        self.assertIsNotNone(sandbox.pr_fields)
        self.assertIn(PR_URL, self.output.getvalue())
        self.network.assert_not_called()

    def test_existing_or_newer_upstream_version_is_a_no_op(self):
        for version in ((1, 0, 26), (1, 0, 27), (1, 10, 0)):
            with self.subTest(version=version):
                sandbox = PublicationSandbox(self)
                sandbox.versions = [(1, 0, 20), version]
                self.submit(sandbox)
                self.assertEqual(sandbox.mutations, [])
                self.assertEqual(sandbox.validated, {})

    def test_same_open_pr_is_reused_without_writes(self):
        sandbox = PublicationSandbox(self)
        sandbox.prs = [pull_request()]
        self.submit(sandbox)
        self.assertEqual(sandbox.mutations, [])
        self.assertEqual(sandbox.validated, {})
        self.assertIn(PR_URL, self.output.getvalue())

    def test_closed_duplicate_multiple_duplicates_and_overlap_stop_without_writes(self):
        for prs in ([pull_request(state="closed")], [pull_request(), pull_request()],
                    [pull_request(version="1.0.25")]):
            with self.subTest(prs=prs):
                sandbox = PublicationSandbox(self)
                sandbox.prs = prs
                with self.assertRaises(ValueError):
                    self.submit(sandbox)
                self.assertEqual(sandbox.mutations, [])
                self.assertEqual(sandbox.validated, {})

    def test_closed_other_version_does_not_match_by_substring(self):
        sandbox = PublicationSandbox(self)
        sandbox.prs = [pull_request(version="1.0.260", state="closed")]
        self.submit(sandbox)
        self.assertIsNotNone(sandbox.pr_fields)

    def test_incomplete_or_truncated_duplicate_search_stops_without_writes(self):
        for complete, count in ((False, 0), (True, 101)):
            with self.subTest(complete=complete, count=count):
                sandbox = PublicationSandbox(self)
                sandbox.search_complete, sandbox.search_count = complete, count
                with self.assertRaises((ValueError, RuntimeError)):
                    self.submit(sandbox)
                self.assertEqual(sandbox.mutations, [])
                self.assertEqual(sandbox.validated, {})

    def test_malformed_duplicate_search_never_counts_as_no_existing_pr(self):
        for data in ({}, {"items": [], "total_count": 0},
                     {"items": {}, "total_count": 0, "incomplete_results": False}):
            with self.subTest(data=data), patch.object(winget, "api", return_value=data), \
                    self.assertRaises((ValueError, RuntimeError)):
                winget.open_prs()
        self.process.assert_not_called()

    def test_read_permission_error_never_reaches_any_process_or_write(self):
        with patch.object(winget, "api", side_effect=PermissionError("HTTP 403")), \
                self.assertRaises(PermissionError):
            winget.submit(VERSION, release(), SHA256)
        self.process.assert_not_called()

    def test_validation_failure_stops_before_branch_creation(self):
        sandbox = PublicationSandbox(self)
        sandbox.validation_failure = True
        with self.assertRaisesRegex(RuntimeError, "winget validate failed"):
            self.submit(sandbox)
        self.assertEqual(sandbox.mutations, [])

    def test_missing_validator_never_falls_back_to_unvalidated_writes(self):
        sandbox = PublicationSandbox(self)
        self.process.side_effect = FileNotFoundError("winget")
        with patch.object(winget, "api", side_effect=sandbox.read_api), self.assertRaises(FileNotFoundError):
            winget.submit(VERSION, release(), SHA256)
        self.assertEqual(self.process.call_count, 1)
        self.assertEqual(self.process.call_args.args[0][0], "winget")

    def test_release_becoming_stale_after_validation_stops_before_any_write(self):
        sandbox = PublicationSandbox(self)
        sandbox.latest.update(id=27, tag_name="v1.0.27")
        with self.assertRaisesRegex(ValueError, "stale"):
            self.submit(sandbox)
        self.assertEqual(sandbox.mutations, [])
        self.assertTrue(sandbox.validated)

    def test_release_becoming_stale_during_uploads_never_opens_a_pr(self):
        sandbox = PublicationSandbox(self)
        sandbox.after_upload = lambda: sandbox.latest.update(id=27, tag_name="v1.0.27")
        with self.assertRaisesRegex(ValueError, "stale"):
            self.submit(sandbox)
        self.assertEqual(sandbox.uploaded, sandbox.validated)
        self.assertEqual(len(sandbox.mutations), 4)
        self.assertIsNone(sandbox.pr_fields)

    def test_concurrent_upstream_pr_created_during_uploads_is_not_duplicated(self):
        sandbox = PublicationSandbox(self)
        sandbox.after_upload = lambda: sandbox.prs.append(pull_request())
        self.submit(sandbox)
        self.assertEqual(sandbox.uploaded, sandbox.validated)
        self.assertEqual(len(sandbox.mutations), 4)
        self.assertIsNone(sandbox.pr_fields)
        self.assertIn(PR_URL, self.output.getvalue())

    def test_version_published_during_uploads_prevents_a_redundant_pr(self):
        sandbox = PublicationSandbox(self)
        sandbox.after_upload = lambda: sandbox.versions.append((1, 0, 26))
        self.submit(sandbox)
        self.assertEqual(len(sandbox.mutations), 4)
        self.assertIsNone(sandbox.pr_fields)

    def test_branch_permission_failure_or_collision_never_writes_contents_or_pr(self):
        for failure, error in (("HTTP 403: Resource not accessible", RuntimeError),
                               ("Reference already exists", ValueError)):
            with self.subTest(failure=failure):
                sandbox = PublicationSandbox(self)
                sandbox.branch_failure = failure
                with self.assertRaises(error):
                    self.submit(sandbox)
                self.assertEqual(sandbox.mutations, [("POST", "repos/qqrm/winget-pkgs/git/refs")])
                self.assertEqual(sandbox.uploaded, {})
                self.assertIsNone(sandbox.pr_fields)

    def test_any_content_permission_failure_stops_before_pr_creation(self):
        for index in (1, 2, 3):
            with self.subTest(index=index):
                sandbox = PublicationSandbox(self)
                sandbox.content_failure = index
                with self.assertRaisesRegex(RuntimeError, "403"):
                    self.submit(sandbox)
                self.assertEqual(len(sandbox.uploaded), index - 1)
                self.assertIsNone(sandbox.pr_fields)
                self.assertFalse(any(endpoint.endswith("/pulls") for _, endpoint in sandbox.mutations))

    def test_pr_permission_failure_does_not_retry_or_attempt_a_merge(self):
        sandbox = PublicationSandbox(self)
        sandbox.pr_failure = True
        with self.assertRaisesRegex(RuntimeError, "403"):
            self.submit(sandbox)
        self.assertEqual(len(sandbox.mutations), 5)
        self.assertEqual(sandbox.mutations[-1], ("POST", "repos/microsoft/winget-pkgs/pulls"))

    def test_missing_publication_token_stops_before_reads(self):
        with patch.dict(os.environ, {"GH_TOKEN": ""}), self.assertRaisesRegex(ValueError, "WINGET_TOKEN"):
            winget.main([])
        self.network.assert_not_called()
        self.process.assert_not_called()

    def test_validate_only_downloads_hashes_and_validates_without_a_token_or_write(self):
        sandbox = PublicationSandbox(self)
        self.process.side_effect = sandbox.run
        self.network.side_effect = lambda request, **kwargs: Response(executable())
        with patch.dict(os.environ, {"GH_TOKEN": ""}), \
                patch.object(winget, "api", side_effect=sandbox.read_api), \
                patch.object(winget, "submit", side_effect=AssertionError("Validation must not publish")):
            winget.main(["--validate-only"])
        self.assertEqual(sandbox.mutations, [])
        self.assertEqual(set(sandbox.validated), set(sandbox.files))
        self.assertEqual(self.process.call_count, 1)

    def test_main_runs_verified_release_through_safe_publication_boundary(self):
        sandbox = PublicationSandbox(self)
        self.process.side_effect = sandbox.run
        self.network.side_effect = lambda request, **kwargs: Response(executable())
        with patch.object(winget, "api", side_effect=sandbox.read_api):
            winget.main([])
        self.assertEqual(sandbox.uploaded, sandbox.validated)
        self.assertEqual(len(sandbox.mutations), 5)
        self.assertIsNotNone(sandbox.pr_fields)


if __name__ == "__main__":
    unittest.main()
