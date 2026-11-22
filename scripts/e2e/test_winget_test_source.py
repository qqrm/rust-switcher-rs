"""Focused protocol and verified-TLS tests for the one-package WinGet source."""
from __future__ import annotations

import http.client
import importlib.util
import json
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import unittest

import yaml

SCRIPT = Path(__file__).with_name("winget-test-source.py")
SPEC = importlib.util.spec_from_file_location("winget_test_source", SCRIPT)
SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOURCE)
PACKAGE = "QQRM.SourceFixture"
LEGACY = PACKAGE + "_Microsoft.Winget.Source_8wekyb3d8bbwe"
CURRENT = PACKAGE + "_is1"


def write_manifests(root: Path, version: str = "999.0.1", *, package: str = PACKAGE, channel: str = "") -> Path:
    directory = root / (version + ("-" + channel if channel else ""))
    directory.mkdir(parents=True)
    identity = {"PackageIdentifier": package, "PackageVersion": version, "ManifestVersion": "1.28.0"}
    if channel:
        identity["Channel"] = channel
    docs = {
        "version": {**identity, "DefaultLocale": "en-US", "ManifestType": "version"},
        "installer": {**identity, "ManifestType": "installer", "InstallerType": "inno", "Scope": "user",
            "UpgradeBehavior": "install", "MinimumOSVersion": "10.0.22000.0", "ReleaseDate": "2026-11-22",
            "AppsAndFeaturesEntries": [
                {"DisplayName": "Source Fixture", "Publisher": "QQRM", "ProductCode": CURRENT, "InstallerType": "inno"},
                {"ProductCode": LEGACY, "InstallerType": "portable"},
            ], "Dependencies": {"PackageDependencies": [{"PackageIdentifier": "Microsoft.VCRedist.2015+.x64"}]},
            "Installers": [{"Architecture": "x64", "InstallerUrl": "http://127.0.0.1:41823/setup.exe",
                "InstallerSha256": "a1" * 32}]},
        "locale": {**identity, "ManifestType": "defaultLocale", "PackageLocale": "en-US", "Publisher": "QQRM",
            "PackageName": "Source Fixture", "ShortDescription": "Fixture only", "License": "MIT",
            "Moniker": "source-fixture", "Tags": ["windows", "on"]},
    }
    for name, doc in docs.items():
        text = yaml.safe_dump(doc, sort_keys=False).replace("'2026-11-22'", "2026-11-22")
        (directory / (name + ".yaml")).write_text(text, encoding="utf-8")
    return directory


def filter_request(field: str, value: str, *, kind: str = "Exact", collection: str = "Filters") -> dict:
    return {collection: [{"PackageMatchField": field, "RequestMatch": {"KeyWord": value, "MatchType": kind}}]}


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "manifests"
        self.directory = write_manifests(self.root)

    def test_production_fields_are_preserved_and_inherited(self):
        catalog = SOURCE.load_catalog(self.root)
        installer = catalog["Versions"][0]["Installers"][0]
        original = yaml.load((self.directory / "installer.yaml").read_text(), Loader=SOURCE.ManifestLoader)
        expected = {key: value for key, value in original.items()
                    if key not in SOURCE.DOCUMENT_KEYS | {"Installers"}}
        expected.update(original["Installers"][0])
        self.assertEqual(installer, expected)
        self.assertEqual(installer["AppsAndFeaturesEntries"][1]["InstallerType"], "portable")
        self.assertEqual(SOURCE.product_codes(catalog["Versions"][0]), sorted([CURRENT, LEGACY]))
        self.assertEqual(installer["ReleaseDate"], "2026-11-22")
        self.assertEqual(catalog["Versions"][0]["DefaultLocale"]["Tags"], ["windows", "on"])
        self.assertNotIn("ManifestType", installer)
        json.dumps(catalog)

    def test_installer_override_replaces_root_field_without_losing_other_defaults(self):
        path = self.directory / "installer.yaml"
        doc = yaml.safe_load(path.read_text())
        doc["Installers"][0]["AppsAndFeaturesEntries"] = [{"ProductCode": "different", "InstallerType": "inno"}]
        doc["Installers"][0]["Scope"] = "machine"
        path.write_text(yaml.safe_dump(doc), encoding="utf-8")
        installer = SOURCE.load_catalog(self.root)["Versions"][0]["Installers"][0]
        self.assertEqual(installer["AppsAndFeaturesEntries"], doc["Installers"][0]["AppsAndFeaturesEntries"])
        self.assertEqual(installer["Scope"], "machine")
        self.assertEqual(installer["UpgradeBehavior"], "install")
        self.assertEqual(installer["Dependencies"], doc["Dependencies"])

    def test_rejects_incomplete_mixed_identity_and_second_package(self):
        installer_path = self.directory / "installer.yaml"
        original = installer_path.read_text()
        installer_path.unlink()
        with self.assertRaises(SOURCE.FixtureError):
            SOURCE.load_catalog(self.root)
        installer_path.write_text(original.replace("PackageVersion: 999.0.1", "PackageVersion: 999.0.2"))
        with self.assertRaises(SOURCE.FixtureError):
            SOURCE.load_catalog(self.root)
        installer_path.write_text(original)
        write_manifests(self.root, "999.0.2", package="Unrelated.Package")
        with self.assertRaises(SOURCE.FixtureError):
            SOURCE.load_catalog(self.root)

    def test_does_not_repair_or_recalculate_bad_digest(self):
        path = self.directory / "installer.yaml"
        path.write_text(path.read_text().replace("a1" * 32, "invalid"))
        with self.assertRaises(SOURCE.FixtureError):
            SOURCE.load_catalog(self.root)

    def test_unquoted_numeric_digest_retains_exact_text_and_leading_zeroes(self):
        path = self.directory / "installer.yaml"
        expected = "01234567" * 8
        path.write_text(path.read_text().replace("a1" * 32, expected))
        installer = SOURCE.load_catalog(self.root)["Versions"][0]["Installers"][0]
        self.assertEqual(installer["InstallerSha256"], expected)

    def test_search_matches_real_ids_codes_and_obeys_filters(self):
        catalog = SOURCE.load_catalog(self.root)
        for field, value in (("PackageIdentifier", PACKAGE), ("ProductCode", LEGACY), ("ProductCode", CURRENT)):
            with self.subTest(field=field, value=value):
                result = SOURCE.search_catalog(catalog, filter_request(field, value))
                self.assertEqual(result["Data"][0]["PackageIdentifier"], PACKAGE)
                self.assertEqual(result["Data"][0]["Versions"][0]["ProductCodes"], sorted([CURRENT, LEGACY]))
        self.assertEqual(SOURCE.search_catalog(catalog, filter_request("ProductCode", "Other_is1")), {"Data": []})
        self.assertEqual(SOURCE.search_catalog(catalog, filter_request("PackageIdentifier", "Other.Package")), {"Data": []})
        request = filter_request("ProductCode", LEGACY, collection="Inclusions")
        request.update(filter_request("PackageIdentifier", "Other.Package"))
        self.assertEqual(SOURCE.search_catalog(catalog, request), {"Data": []})
        request = filter_request("NormalizedPackageNameAndPublisher", "Source Fixture", collection="Inclusions")
        request["Inclusions"].extend(filter_request("ProductCode", LEGACY, collection="Inclusions")["Inclusions"])
        self.assertEqual(len(SOURCE.search_catalog(catalog, request)["Data"]), 1)
        self.assertEqual(len(SOURCE.search_catalog(catalog, filter_request("PackageIdentifier", PACKAGE.lower(), kind="CaseInsensitive"))["Data"]), 1)

    def test_malformed_and_unsupported_searches_fail_closed(self):
        catalog = SOURCE.load_catalog(self.root)
        requests = [{}, {"MaximumResults": True, "FetchAllManifests": True}, {"FetchAllManifests": "true"},
                    {"Filters": "wrong"}, {"Unknown": True}, filter_request("Unknown", "x"),
                    filter_request("PackageIdentifier", PACKAGE, kind="Fuzzy")]
        for request in requests:
            with self.subTest(request=request), self.assertRaises(SOURCE.FixtureError):
                SOURCE.search_catalog(catalog, request)
        combined = filter_request("PackageIdentifier", PACKAGE, collection="Inclusions")
        combined["Inclusions"].append({"PackageMatchField": "Unknown", "RequestMatch": {"KeyWord": "x", "MatchType": "Exact"}})
        with self.assertRaises(SOURCE.FixtureError):
            SOURCE.search_catalog(catalog, combined)

    def test_empty_serialized_keyword_is_no_match_and_fetch_all_is_explicit(self):
        catalog = SOURCE.load_catalog(self.root)
        self.assertEqual(SOURCE.search_catalog(catalog, {"Query": {"KeyWord": "", "MatchType": "Substring"}}), {"Data": []})
        self.assertEqual(SOURCE.search_catalog(catalog, filter_request("NormalizedPackageNameAndPublisher", "")), {"Data": []})
        self.assertEqual(len(SOURCE.search_catalog(catalog, {"FetchAllManifests": True, "MaximumResults": 1})["Data"]), 1)

    def test_startup_failure_is_logged_without_private_input_paths(self):
        ready, stop, log = self.root / "ready.json", self.root / "stop", self.root / "source.log"
        missing_certificate = self.root / "private-certificate-location.pem"
        missing_key = self.root / "private-key-location.pem"
        result = subprocess.run([sys.executable, str(SCRIPT), "--manifest-root", str(self.root),
            "--cert-file", str(missing_certificate), "--key-file", str(missing_key),
            "--source-id", "QQRM-CI-test", "--ready-file", str(ready), "--stop-file", str(stop),
            "--log-file", str(log)], capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(ready.exists())
        diagnostics = log.read_text() + result.stdout + result.stderr
        self.assertIn("fixture-failed", diagnostics)
        self.assertNotIn(str(missing_key), diagnostics)
        self.assertNotIn(str(missing_certificate), diagnostics)


@unittest.skipUnless(shutil.which("openssl"), "openssl is required to create temporary verified TLS certificates")
class HttpsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        self.root = self.work / "manifests"
        write_manifests(self.root)
        self.cert, self.key = self.work / "localhost.pem", self.work / "localhost-key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                        "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
                        "-keyout", str(self.key), "-out", str(self.cert)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=15)
        self.ready, self.stop, self.log = self.work / "ready.json", self.work / "stop", self.work / "source.log"
        self.process = subprocess.Popen([sys.executable, str(SCRIPT), "--manifest-root", str(self.root),
            "--port", "0", "--cert-file", str(self.cert), "--key-file", str(self.key),
            "--source-id", "QQRM-CI-test", "--ready-file", str(self.ready), "--stop-file", str(self.stop),
            "--log-file", str(self.log)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(self.stop_server)
        deadline = time.monotonic() + 8
        while not self.ready.exists() and self.process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.025)
        if not self.ready.exists():
            self.fail("HTTPS fixture did not become ready")
        self.metadata = json.loads(self.ready.read_text())
        self.context = ssl.create_default_context(cafile=str(self.cert))

    def stop_server(self):
        self.stop.touch()
        try:
            self.process.communicate(timeout=4)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.communicate(timeout=4)
            self.fail("HTTPS fixture exceeded bounded shutdown")

    def request(self, method, path, body=None, *, contract=True):
        connection = http.client.HTTPSConnection("localhost", self.metadata["port"], context=self.context, timeout=3)
        headers = {"Version": SOURCE.CONTRACT} if contract else {}
        if body is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(body).encode()
        try:
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            content_type = response.getheader("Content-Type")
            data = json.loads(response.read())
            self.assertTrue(content_type.startswith("application/json"))
            return response.status, data
        finally:
            connection.close()

    def test_information_manifest_filters_and_dynamic_version(self):
        self.assertEqual(self.metadata["pid"], self.process.pid)
        self.assertEqual(self.metadata["sourceIdentifier"], "QQRM-CI-test")
        status, info = self.request("GET", "/information", contract=False)
        self.assertEqual((status, info["Data"]["ServerSupportedVersions"]), (200, [SOURCE.CONTRACT]))
        status, body = self.request("GET", f"/packageManifests/{PACKAGE}?Version=999.0.1&Channel=")
        self.assertEqual(status, 200)
        self.assertEqual(body["Data"]["Versions"][0]["Installers"][0]["AppsAndFeaturesEntries"][1]["ProductCode"], LEGACY)
        for query in ("Version=999.0.2", "Version=999.0.1&Channel=beta", "Version=999.0", "Version="):
            self.assertEqual(self.request("GET", f"/packageManifests/{PACKAGE}?{query}")[0], 404)
        self.assertEqual(self.request("GET", "/packageManifests/Other.Package")[0], 404)
        self.assertEqual(self.request("GET", f"/packageManifests/{PACKAGE}?Version=999.0.1&Version=999.0.2")[0], 400)
        self.assertEqual(self.request("GET", f"/packageManifests/{PACKAGE}", contract=False)[0], 400)
        stage_root = self.work / "stage"
        staged = write_manifests(stage_root, "999.0.2", channel="beta")
        staged.rename(self.root / staged.name)
        status, body = self.request("GET", f"/packageManifests/{PACKAGE}?Version=999.0.2&Channel=beta")
        self.assertEqual(status, 200)
        self.assertEqual(len(body["Data"]["Versions"]), 1)
        self.assertEqual(body["Data"]["Versions"][0]["Channel"], "beta")

    def test_actual_post_search_and_invalid_request(self):
        status, body = self.request("POST", "/manifestSearch", filter_request("ProductCode", LEGACY))
        self.assertEqual(status, 200)
        self.assertEqual(body["Data"][0]["Versions"][0]["ProductCodes"], sorted([CURRENT, LEGACY]))
        self.assertEqual(self.request("POST", "/manifestSearch", filter_request("ProductCode", "unrelated")), (200, {"Data": []}))
        self.assertEqual(self.request("POST", "/manifestSearch", {"FetchAllManifests": True})[0], 200)
        self.assertEqual(self.request("POST", "/manifestSearch", {"Filters": "wrong"})[0], 400)
        self.assertEqual(self.request("GET", "/unknown")[0], 404)
        incomplete = self.root / "incomplete"
        incomplete.mkdir()
        shutil.copy2(next(self.root.rglob("version.yaml")), incomplete / "version.yaml")
        self.assertEqual(self.request("GET", f"/packageManifests/{PACKAGE}")[0], 503)

    def test_tls_requires_trust_and_idle_client_does_not_block_stop(self):
        connection = http.client.HTTPSConnection("localhost", self.metadata["port"], context=ssl.create_default_context(), timeout=3)
        try:
            with self.assertRaises(ssl.SSLCertVerificationError):
                connection.request("GET", "/information")
        finally:
            connection.close()
        idle = socket.create_connection(("127.0.0.1", self.metadata["port"]), timeout=2)
        self.addCleanup(idle.close)
        status, _ = self.request("GET", "/information", contract=False)
        self.assertEqual(status, 200)
        started = time.monotonic()
        self.stop_server()
        self.assertLess(time.monotonic() - started, 2)
        self.assertEqual(self.process.returncode, 0)
        self.assertFalse(self.ready.exists())
        log = self.log.read_text()
        self.assertIn('"event":"stopped"', log)
        self.assertNotIn("PRIVATE KEY", log)
        self.assertNotIn(str(self.key), log)
        self.assertNotIn(str(self.cert), log)


if __name__ == "__main__":
    unittest.main()
