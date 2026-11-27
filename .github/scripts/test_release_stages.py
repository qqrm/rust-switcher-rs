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


def version_pr_response(head=COMMIT, state="OPEN", **changes):
    pr = {
        "number": 7, "headRefOid": head, "state": state,
        "headRefName": "automation/release-" + SOURCE, "baseRefName": "dev",
        "isCrossRepository": False, "viewerDidAuthor": True,
        "headRepository": {"id": "repository-id"},
    }
    pr.update(changes)
    return {"data": {"repository": {"id": "repository-id", "pullRequest": pr}}}


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


    def test_branch_only_retry_reuses_remote_commit_after_pr_creation_failure(self):
        remote, proposed = "c" * 40, "d" * 40
        branch = "automation/release-" + SOURCE
        seen_head = 0

        def fake_git(*args):
            nonlocal seen_head
            if args == ("rev-parse", "HEAD"):
                seen_head += 1
                return SOURCE if seen_head == 1 else proposed
            if "ls-remote" in args:
                return remote + "\trefs/heads/" + branch
            if args[0] == "rev-list":
                return remote + " " + SOURCE
            if args[0] == "log":
                return f"chore: release {VERSION}\n\nrelease-source: {SOURCE}"
            return ""

        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            directory = root / "plan"
            directory.mkdir()
            for name in release.VERSION_FILES:
                target = directory / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("version data")
                (root / name).parent.mkdir(parents=True, exist_ok=True)
            plan = {"source": SOURCE, "version": VERSION, "merged": ""}
            args = argparse.Namespace(directory=str(directory), source=SOURCE)
            with patch.object(release, "ROOT", root), \
                    patch.object(release, "validate_plan", return_value=plan), \
                    patch.object(release, "git", side_effect=fake_git) as git, \
                    patch.object(release, "gh", side_effect=[
                        [], {"number": 7}, version_pr_response(head=remote)]), \
                    patch.object(release, "command") as command, \
                    patch.object(release, "output") as output, \
                    patch.dict(os.environ, {"GITHUB_REPOSITORY": "owner/repo"}):
                release.version_pr(args)
                git.assert_any_call("diff", "--exit-code", proposed, remote)
                self.assertFalse(any("push" in call.args for call in git.call_args_list))
                command.assert_any_call(
                    "gh", "pr", "merge", "7", "--repo", "owner/repo", "--auto", "--squash",
                    "--match-head-commit", remote, "--subject", f"chore: release {VERSION}",
                    "--body", f"release-source: {SOURCE}")
                output.assert_called_once_with(merged="", pr=7, head=remote, version=VERSION)

    def test_branch_only_retry_refuses_unexpected_parent_message_or_tree(self):
        remote, proposed = "c" * 40, "d" * 40
        branch = "automation/release-" + SOURCE
        for invalid in ("parent", "message", "tree"):
            with self.subTest(invalid=invalid):
                def fake_git(*args):
                    if "ls-remote" in args:
                        return remote + "\trefs/heads/" + branch
                    if args[0] == "rev-list":
                        return remote + " " + (proposed if invalid == "parent" else SOURCE)
                    if args[0] == "log":
                        return "other release" if invalid == "message" else (
                            f"chore: release {VERSION}\n\nrelease-source: {SOURCE}")
                    if args[0] == "diff" and invalid == "tree":
                        raise RuntimeError("different tree")
                    return ""

                with patch.object(release, "git", side_effect=fake_git) as git:
                    with self.assertRaises((ValueError, RuntimeError)):
                        release.recover_pushed_branch(branch, SOURCE, VERSION, proposed)
                    self.assertFalse(any("push" in call.args for call in git.call_args_list))


class VersionPrIdentityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        directory = root / "plan"
        directory.mkdir()
        for name in release.VERSION_FILES:
            target = directory / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text("version data")
            (root / name).parent.mkdir(parents=True, exist_ok=True)
        self.args = argparse.Namespace(directory=str(directory), source=SOURCE)
        self.branch = "automation/release-" + SOURCE
        self.proposed = "c" * 40
        self.head_reads = 0
        self.changed_tree = False

        def fake_git(*args):
            if args == ("rev-parse", "HEAD"):
                self.head_reads += 1
                return SOURCE if self.head_reads == 1 else self.proposed
            if args[0] == "diff" and self.changed_tree:
                raise RuntimeError("different tree")
            return ""

        patches = {
            "ROOT": patch.object(release, "ROOT", root),
            "plan": patch.object(release, "validate_plan", return_value={
                "source": SOURCE, "version": VERSION, "merged": ""}),
            "git": patch.object(release, "git", side_effect=fake_git),
            "gh": patch.object(release, "gh"),
            "command": patch.object(release, "command"),
            "output": patch.object(release, "output"),
            "environment": patch.dict(os.environ, {"GITHUB_REPOSITORY": "qqrm/rust-switcher-rs"}),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)

    def test_existing_writer_pr_keeps_exact_tree_and_head_checks(self):
        # viewerDidAuthor is evaluated by GitHub for the current RELEASE_TOKEN;
        # recent production version PRs use qqrm's PAT, not the git bot author.
        self.gh.side_effect = [[{"number": 7}], version_pr_response()]
        release.version_pr(self.args)
        self.git.assert_any_call("fetch", "origin", COMMIT)
        self.git.assert_any_call("diff", "--exit-code", self.proposed, COMMIT)
        self.command.assert_called_once_with(
            "gh", "pr", "merge", "7", "--repo", "qqrm/rust-switcher-rs", "--auto", "--squash",
            "--match-head-commit", COMMIT, "--subject", f"chore: release {VERSION}",
            "--body", f"release-source: {SOURCE}")
        query = self.gh.call_args_list[1].args
        self.assertEqual(query[:2], ("api", "graphql"))
        self.assertIn("owner=qqrm", query)
        self.assertIn("name=rust-switcher-rs", query)
        self.assertIn("number=7", query)

    def test_merged_writer_pr_can_resume_without_another_merge(self):
        self.gh.side_effect = [[{"number": 7}], version_pr_response(state="MERGED")]
        release.version_pr(self.args)
        self.git.assert_any_call("diff", "--exit-code", self.proposed, COMMIT)
        self.command.assert_not_called()
        self.output.assert_called_once_with(merged="", pr=7, head=COMMIT, version=VERSION)

    def test_untrusted_or_incomplete_identity_never_fetches_or_merges(self):
        invalid = [
            {"viewerDidAuthor": False}, {"viewerDidAuthor": None},
            {"isCrossRepository": True}, {"isCrossRepository": None},
            {"headRepository": {"id": "fork-id"}}, {"headRepository": None},
            {"baseRefName": "main"}, {"headRefName": "another-branch"},
            {"number": 8}, {"state": "CLOSED"}, {"state": None},
        ]
        for changes in invalid:
            with self.subTest(changes=changes):
                self.head_reads = 0
                self.git.reset_mock()
                self.gh.side_effect = [[{"number": 7}], version_pr_response(**changes)]
                with self.assertRaises(ValueError):
                    release.version_pr(self.args)
                self.assertFalse(any(call.args[0] == "fetch" for call in self.git.call_args_list))
                self.command.assert_not_called()
                self.output.assert_not_called()

    def test_missing_repository_or_pr_fails_closed(self):
        for data in ({}, {"data": {}}, {"data": {"repository": None}},
                     {"data": {"repository": {"id": "repository-id", "pullRequest": None}}},
                     {"data": {"repository": {"pullRequest": version_pr_response()["data"]["repository"]["pullRequest"]}}}):
            with self.subTest(data=data), patch.object(release, "gh", return_value=data):
                with self.assertRaises(ValueError):
                    release.checked_version_pr("qqrm/rust-switcher-rs", 7, self.branch)

    def test_authenticated_writer_with_wrong_tree_is_still_refused(self):
        self.changed_tree = True
        self.gh.side_effect = [[{"number": 7}], version_pr_response()]
        with self.assertRaises(RuntimeError):
            release.version_pr(self.args)
        self.command.assert_not_called()
        self.output.assert_not_called()

    def test_new_version_pr_is_validated_before_merge(self):
        self.gh.side_effect = [[], {"number": 7}, version_pr_response(head=self.proposed)]
        release.version_pr(self.args)
        self.git.assert_any_call(
            "-c", "credential.helper=!gh auth git-credential", "push", "origin",
            "HEAD:refs/heads/" + self.branch)
        self.assertEqual([call.args[:3] for call in self.command.call_args_list],
                         [("gh", "pr", "create"), ("gh", "pr", "merge")])
        self.output.assert_called_once_with(merged="", pr=7, head=self.proposed, version=VERSION)

    def test_new_version_pr_with_wrong_identity_or_head_is_not_merged(self):
        for changes in ({"viewerDidAuthor": False}, {"headRepository": {"id": "fork-id"}},
                        {"headRefOid": COMMIT}):
            with self.subTest(changes=changes):
                self.head_reads = 0
                self.command.reset_mock()
                response = version_pr_response(head=self.proposed)
                response["data"]["repository"]["pullRequest"].update(changes)
                self.gh.side_effect = [[], {"number": 7}, response]
                with self.assertRaises(ValueError):
                    release.version_pr(self.args)
                self.assertEqual([call.args[:3] for call in self.command.call_args_list],
                                 [("gh", "pr", "create")])
                self.output.assert_not_called()


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

    def test_each_upload_regenerates_without_token_then_reconciles_response_loss(self):
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

            def fake_command(*args, **_kwargs):
                if "publish" in args:
                    raise RuntimeError("response lost")
                return ""

            with patch.object(release, "validate_bundle", return_value=manifest), \
                    patch.object(release, "git", return_value=COMMIT), \
                    patch.object(release, "index_checksum", side_effect=expected), \
                    patch.object(release, "command", side_effect=fake_command) as command, \
                    patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "test-only", "GH_TOKEN": "must-not-leak"}), \
                    patch.object(release.shutil, "which", return_value="cargo"):
                release.publish_crates(args)
                self.assertEqual(command.call_count, 4)
                self.assertEqual([call.args[2] for call in command.call_args_list],
                                 ["package", "publish", "package", "publish"])
                for call in command.call_args_list:
                    self.assertIn("--no-verify", call.args)
                    self.assertIn("--locked", call.args)
                    self.assertEqual(call.kwargs["cwd"], sterile)
                    self.assertNotIn("GH_TOKEN", call.kwargs["env"])
                    if call.args[2] == "package":
                        self.assertNotIn("CARGO_REGISTRY_TOKEN", call.kwargs["env"])
                    else:
                        self.assertEqual(call.kwargs["env"]["CARGO_REGISTRY_TOKEN"], "test-only")

    def test_individual_package_divergence_stops_before_any_upload(self):
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

            def repackage(*command, **kwargs):
                self.assertEqual(command[2], "package")
                self.assertNotIn("CARGO_REGISTRY_TOKEN", kwargs["env"])
                self.assertEqual(command[3:7], ("--locked", "--no-verify", "--registry", "crates-io"))
                (sterile / "target/package" / f"rust-switcher-core-{VERSION}.crate").write_bytes(b"different")

            with patch.object(release, "validate_bundle", return_value=manifest), \
                    patch.object(release, "git", return_value=COMMIT), \
                    patch.object(release, "index_checksum", return_value=None), \
                    patch.object(release, "command", side_effect=repackage) as command, \
                    patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "test-only"}), \
                    patch.object(release.shutil, "which", return_value="cargo"):
                with self.assertRaisesRegex(ValueError, "Individual package differs"):
                    release.publish_crates(args)
                self.assertEqual(command.call_count, 1)


    def test_dependency_retry_repackages_without_token_before_each_upload(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            sha, manifest = write_bundle(directory)
            sterile = directory / "sterile"
            (sterile / "target/package").mkdir(parents=True)
            checksums = []
            for package in release.PACKAGES:
                filename = f"{package}-{VERSION}.crate"
                (sterile / "target/package" / filename).write_bytes((directory / filename).read_bytes())
                checksums.append(manifest["files"][filename]["sha256"])
            args = argparse.Namespace(directory=str(directory), source=SOURCE, commit=COMMIT,
                                      version=VERSION, manifest_sha=sha, sterile=str(sterile))
            uploads = 0

            def fake_command(*command, **kwargs):
                nonlocal uploads
                if command[2] == "package":
                    self.assertNotIn("CARGO_REGISTRY_TOKEN", kwargs["env"])
                else:
                    uploads += 1
                    if uploads == 1:
                        raise RuntimeError("no matching package named rust-switcher-core")
                return ""

            with patch.object(release, "validate_bundle", return_value=manifest), \
                    patch.object(release, "git", return_value=COMMIT), \
                    patch.object(release, "index_checksum", side_effect=[checksums[0], None, None, checksums[1]]), \
                    patch.object(release, "command", side_effect=fake_command) as command, \
                    patch.object(release.time, "sleep") as sleep, \
                    patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "test-only"}), \
                    patch.object(release.shutil, "which", return_value="cargo"):
                release.publish_crates(args)
                self.assertEqual([call.args[2] for call in command.call_args_list],
                                 ["package", "publish", "package", "publish"])
                sleep.assert_called_once_with(15)


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
        self.assertIn("actions: read", writer)
        self.assertNotIn("cargo ", writer)
        self.assertNotIn("rust-toolchain@", writer)
        self.assertEqual(workflow.count("secrets.RELEASE_TOKEN"), 1)
        self.assertEqual(workflow.count("persist-credentials: false"), 5)
        publisher = workflow.split("\n  publish:\n", 1)[1]
        self.assertLess(publisher.index("preflight --"), publisher.index("id: crates_auth"))
        self.assertIn("artifact-ids:", publisher)
        self.assertNotIn("rust-cache@", publisher)
        self.assertNotIn("--admin", workflow)
        self.assertNotIn("--admin", (release.ROOT / ".github/scripts/release_stages.py").read_text())


if __name__ == "__main__":
    unittest.main()
