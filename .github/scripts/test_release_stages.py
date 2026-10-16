"""Negative tests for release trust boundaries; these tests never publish."""
import argparse
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import release_stages as release

SOURCE = "a" * 40
COMMIT = "b" * 40
VERSION = "1.2.3"
ORIGINAL = {
    "Cargo.toml": '[package]\nname = "rust-switcher"\nversion = "1.0.19"\n'
                  '[dependencies]\nrust-switcher-core = { version = "1.0.19", path = "crates/rust-switcher-core" }\n'
                  'serde = "1"\n',
    "crates/rust-switcher-core/Cargo.toml": '[package]\nname = "rust-switcher-core"\nversion = "1.0.19"\n',
    "Cargo.lock": 'version = 4\n\n[[package]]\nname = "rust-switcher"\nversion = "1.0.19"\n'
                  'dependencies = ["rust-switcher-core", "serde"]\n\n[[package]]\n'
                  'name = "rust-switcher-core"\nversion = "1.0.19"\n\n[[package]]\n'
                  'name = "serde"\nversion = "1.0.228"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'
                  'checksum = "unchanged"\n',
}


def write_plan(directory):
    (directory / "plan.json").write_text(json.dumps({
        "source": SOURCE, "version": VERSION, "merged": "",
    }), encoding="utf-8")
    for name, text in release.version_files(ORIGINAL, VERSION).items():
        path = directory / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


def write_bundle(directory, crate_commit=COMMIT, unsafe=False):
    for filename in release.expected_files(VERSION):
        (directory / filename).write_bytes(b"verified artifact")
    for package in release.PACKAGES:
        with tarfile.open(directory / f"{package}-{VERSION}.crate", "w:gz") as archive:
            prefix = f"{package}-{VERSION}/"
            items = {
                ".cargo_vcs_info.json": json.dumps({"git": {"sha1": crate_commit, "dirty": False}}),
                "Cargo.toml": f'[package]\nname = "{package}"\nversion = "{VERSION}"\n',
            }
            if unsafe:
                items["../escape"] = "refuse"
            for name, content in items.items():
                data = content.encode()
                member = tarfile.TarInfo(prefix + name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
    manifest = {
        "source": SOURCE, "commit": COMMIT, "version": VERSION, "cargo": "cargo test",
        "rustc": "rustc test",
        "files": {path.name: {"size": path.stat().st_size, "sha256": release.digest(path)}
                  for path in directory.iterdir()},
    }
    path = directory / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")
    return release.digest(path), manifest


class VersionPlanTests(unittest.TestCase):
    def test_version_change_preserves_third_party_lock_entries_and_source_input(self):
        original = copy.deepcopy(ORIGINAL)
        result = release.version_files(original, VERSION)
        self.assertEqual(original, ORIGINAL)
        self.assertIn('version = "1.0.228"', result["Cargo.lock"])
        self.assertIn('checksum = "unchanged"', result["Cargo.lock"])
        self.assertEqual(result["Cargo.lock"].count('version = "1.2.3"'), 2)
        self.assertIn('version = "1.2.3", path =', result["Cargo.toml"])

    def test_ambiguous_lock_or_invalid_version_fail_closed(self):
        original = copy.deepcopy(ORIGINAL)
        original["Cargo.lock"] += '\n[[package]]\nname = "rust-switcher"\nversion = "1.0.19"\n'
        with self.assertRaises(ValueError):
            release.version_files(original, VERSION)
        with self.assertRaises(ValueError):
            release.version_files(ORIGINAL, "1.2.3\nextra")

    @patch.object(release, "source_files", return_value=ORIGINAL)
    def test_plan_accepts_only_exact_version_data(self, _source):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            write_plan(directory)
            self.assertEqual(release.validate_plan(directory, SOURCE)["version"], VERSION)
            with self.assertRaises(ValueError):
                release.validate_plan(directory, COMMIT)
            lock = directory / "Cargo.lock"
            lock.write_text(lock.read_text().replace('checksum = "unchanged"', 'checksum = "tampered"'))
            with self.assertRaises(ValueError):
                release.validate_plan(directory, SOURCE)

    @patch.object(release, "source_files", return_value=ORIGINAL)
    def test_plan_refuses_executable_payload(self, _source):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            write_plan(directory)
            (directory / "publish.py").write_text("raise SystemExit(1)")
            with self.assertRaises(ValueError):
                release.validate_plan(directory, SOURCE)

    def test_existing_tag_retry_requires_identical_commit(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            (directory / "plan.json").write_text(json.dumps({
                "source": SOURCE, "version": VERSION, "merged": COMMIT,
            }))
            with patch.object(release, "existing_source_tag", return_value="v" + VERSION), \
                    patch.object(release, "git", return_value=SOURCE):
                with self.assertRaises(ValueError):
                    release.validate_plan(directory, SOURCE)


class PublicationBoundaryTests(unittest.TestCase):
    def test_manifest_and_files_must_match_the_checked_commit(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            sha, manifest = write_bundle(directory)
            self.assertEqual(release.validate_bundle(directory, SOURCE, COMMIT, VERSION, sha), manifest)
            with self.assertRaises(ValueError):
                release.validate_bundle(directory, COMMIT, COMMIT, VERSION, sha)
            (directory / "rust-switcher.exe").write_bytes(b"altered")
            with self.assertRaises(ValueError):
                release.validate_bundle(directory, SOURCE, COMMIT, VERSION, sha)

    def test_crate_vcs_metadata_and_archive_paths_are_verified(self):
        for commit, unsafe in ((SOURCE, False), (COMMIT, True)):
            with self.subTest(commit=commit, unsafe=unsafe), tempfile.TemporaryDirectory() as temp:
                directory = Path(temp)
                sha, _ = write_bundle(directory, crate_commit=commit, unsafe=unsafe)
                with self.assertRaises(ValueError):
                    release.validate_bundle(directory, SOURCE, COMMIT, VERSION, sha)

    def test_publisher_environment_removes_other_tokens_and_cargo_hooks(self):
        inherited = {
            "PATH": "system-tools", "SystemRoot": "windows", "USERPROFILE": "runner",
            "GH_TOKEN": "github-secret", "RELEASE_TOKEN": "writer-secret",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN": "oidc-request-secret",
            "CARGO_HOME": "build-cache", "RUSTC_WRAPPER": "untrusted-wrapper",
            "CARGO_REGISTRIES_CRATES_IO_CREDENTIAL_PROVIDER": "untrusted-provider",
            "CARGO_REGISTRY_TOKEN": "inherited-registry-secret", "RUSTFLAGS": "untrusted",
        }
        with patch.dict(os.environ, inherited, clear=True):
            clean = release.cargo_environment(Path("sterile"))
            authenticated = release.cargo_environment(Path("sterile"), "narrow-registry-token")
        self.assertEqual(clean["PATH"], "system-tools")
        self.assertEqual(clean["SystemRoot"], "windows")
        self.assertEqual(clean["CARGO_HOME"], str(Path("sterile/cargo-home")))
        for name in inherited.keys() - {"PATH", "SystemRoot", "USERPROFILE", "CARGO_HOME"}:
            self.assertNotIn(name, clean)
        self.assertEqual(authenticated.pop("CARGO_REGISTRY_TOKEN"), "narrow-registry-token")
        self.assertEqual(authenticated, clean)

    def test_already_published_different_bytes_never_count_as_success(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            sha, manifest = write_bundle(directory)
            sterile = directory / "sterile"
            (sterile / "target/package").mkdir(parents=True)
            for package in release.PACKAGES:
                filename = f"{package}-{VERSION}.crate"
                (sterile / "target/package" / filename).write_bytes((directory / filename).read_bytes())
            args = argparse.Namespace(directory=str(directory), source=SOURCE, commit=COMMIT,
                                      version=VERSION, manifest_sha=sha, sterile=str(sterile))
            with patch.object(release, "validate_bundle", return_value=manifest), \
                    patch.object(release, "git", return_value=COMMIT), \
                    patch.object(release, "index_checksum", return_value="f" * 64), \
                    patch.object(release, "command") as command, \
                    patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "test-only"}), \
                    patch.object(release.shutil, "which", return_value="cargo"):
                with self.assertRaises(ValueError):
                    release.publish_crates(args)
                command.assert_not_called()

    def test_publish_has_no_compilation_and_reconciles_upload_response_loss(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            sha, manifest = write_bundle(directory)
            sterile = directory / "sterile"
            (sterile / "target/package").mkdir(parents=True)
            expected = []
            for package in release.PACKAGES:
                filename = f"{package}-{VERSION}.crate"
                (sterile / "target/package" / filename).write_bytes((directory / filename).read_bytes())
                expected += [None, manifest["files"][filename]["sha256"]]
            args = argparse.Namespace(directory=str(directory), source=SOURCE, commit=COMMIT,
                                      version=VERSION, manifest_sha=sha, sterile=str(sterile))
            with patch.object(release, "validate_bundle", return_value=manifest), \
                    patch.object(release, "git", return_value=COMMIT), \
                    patch.object(release, "index_checksum", side_effect=expected), \
                    patch.object(release, "command", side_effect=RuntimeError("response lost")) as command, \
                    patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "test-only", "GH_TOKEN": "must-not-leak"}), \
                    patch.object(release.shutil, "which", return_value="cargo"):
                release.publish_crates(args)
                self.assertEqual(command.call_count, 2)
                for call in command.call_args_list:
                    self.assertIn("--no-verify", call.args)
                    self.assertIn("--locked", call.args)
                    self.assertEqual(call.kwargs["cwd"], sterile)
                    self.assertNotIn("GH_TOKEN", call.kwargs["env"])


class WorkflowPolicyTests(unittest.TestCase):
    def test_build_has_no_publication_capabilities_and_writer_never_compiles(self):
        workflow = (release.ROOT / ".github/workflows/release.yml").read_text()
        writer = workflow.split("\n  version-pr:\n", 1)[1].split("\n  build:\n", 1)[0]
        build = workflow.split("\n  build:\n", 1)[1].split("\n  publish:\n", 1)[0]
        self.assertNotIn("secrets.", build)
        self.assertNotIn("environment: release", build)
        self.assertNotIn("id-token:", build)
        self.assertNotIn("contents: write", build)
        self.assertNotIn("GH_TOKEN:", build)
        self.assertNotIn("cargo ", writer)
        self.assertNotIn("rust-toolchain@", writer)
        self.assertEqual(workflow.count("secrets.RELEASE_TOKEN"), 1)
        self.assertEqual(workflow.count("persist-credentials: false"), 4)
        publisher = workflow.split("\n  publish:\n", 1)[1]
        self.assertLess(publisher.index("preflight --"), publisher.index("id: crates_auth"))
        self.assertIn("artifact-ids:", publisher)
        self.assertNotIn("rust-cache@", publisher)
        self.assertNotIn("--admin", workflow)
        self.assertNotIn("--admin", (release.ROOT / ".github/scripts/release_stages.py").read_text())


if __name__ == "__main__":
    unittest.main()
