"""Release data preparation and publication; never compile with publication credentials."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import zipfile

VERSION_FILES = ("Cargo.toml", "Cargo.lock", "crates/rust-switcher-core/Cargo.toml")
PACKAGES = ("rust-switcher-core", "rust-switcher")
ROOT = Path(__file__).resolve().parents[2]


def command(*args, cwd=ROOT, env=None, strip=True):
    result = subprocess.run(args, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stdout}")
    return result.stdout.strip() if strip else result.stdout


def git(*args):
    return command("git", *args)


def gh(*args):
    return json.loads(command("gh", *args))


def checked_sha(value):
    if not re.fullmatch(r"[a-f0-9]{40}", value):
        raise ValueError("Invalid immutable commit")
    return value


def checked_version(value):
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", value):
        raise ValueError("Expected numeric X.Y.Z version")
    return value


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def output(**values):
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
        for name, value in values.items():
            value = str(value)
            if "\n" in value or "\r" in value:
                raise ValueError("Multiline job output refused")
            stream.write(f"{name}={value}\n")


def source_files(source):
    checked_sha(source)
    return {name: command("git", "show", f"{source}:{name}", strip=False) for name in VERSION_FILES}


def version_files(original, version):
    """Only the two workspace versions and their internal dependency may change."""
    checked_version(version)
    result = {}
    for name, text in original.items():
        if name == "Cargo.lock":
            sections = text.split("[[package]]")
            found = set()
            for index in range(1, len(sections)):
                body = sections[index]
                entry = tomllib.loads(body)
                if entry.get("name") in PACKAGES and "source" not in entry:
                    package = entry["name"]
                    if package in found:
                        raise ValueError("Ambiguous workspace lock entry")
                    found.add(package)
                    body, count = re.subn(r'(?m)^version = "[^"]+"$', f'version = "{version}"', body, count=1)
                    if count != 1:
                        raise ValueError("Missing workspace lock version")
                    sections[index] = body
            if found != set(PACKAGES):
                raise ValueError("Missing workspace lock entries")
            updated = "[[package]]".join(sections)
        else:
            match = re.search(r"(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)", text)
            if not match:
                raise ValueError("Missing package table")
            body, count = re.subn(r'(?m)^version = "[^"]+"$', f'version = "{version}"', match[1], count=1)
            if count != 1:
                raise ValueError("Missing package version")
            updated = text[:match.start(1)] + body + text[match.end(1):]
            if name == "Cargo.toml":
                updated, count = re.subn(
                    r'(?m)^(rust-switcher-core\s*=\s*\{[^\n]*?\bversion\s*=\s*")[^"]+(")',
                    lambda match: match[1] + version + match[2], updated, count=1)
                if count != 1:
                    raise ValueError("Missing internal dependency version")
        before, after = tomllib.loads(text), tomllib.loads(updated)
        expected = copy.deepcopy(before)
        if name == "Cargo.lock":
            for entry in expected["package"]:
                if entry["name"] in PACKAGES and "source" not in entry:
                    entry["version"] = version
        else:
            expected["package"]["version"] = version
            if name == "Cargo.toml":
                expected["dependencies"]["rust-switcher-core"]["version"] = version
        if after != expected:
            raise ValueError("Version preparation changed unrelated metadata")
        result[name] = updated
    return result


def existing_source_tag(source):
    matches = []
    for tag in git("tag", "--list", "v*", "--sort=-v:refname").splitlines():
        if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
            continue
        body = git("log", "-1", "--format=%B", tag)
        if re.search(r"(?m)^release-source:\s*" + re.escape(source) + r"\s*$", body):
            matches.append(tag)
    if len(matches) > 1:
        raise ValueError("Multiple release tags map to this source; reconcile first")
    return matches[0] if matches else None


def prepare(args):
    source = checked_sha(args.source)
    if git("rev-parse", "HEAD") != source:
        raise ValueError("Preparation checkout does not match source")
    original = source_files(source)
    tag = existing_source_tag(source)
    if tag:
        version = checked_version(tag[1:])
        if args.version and checked_version(args.version) != version:
            raise ValueError("Source already maps to another version")
        merged = checked_sha(git("rev-parse", f"{tag}^{{commit}}"))
        for name in ("Cargo.toml", "crates/rust-switcher-core/Cargo.toml"):
            if tomllib.loads(command("git", "show", f"{merged}:{name}"))["package"]["version"] != version:
                raise ValueError("Existing release tag has inconsistent versions")
    else:
        versions = [tomllib.loads(original[name])["package"]["version"]
                    for name in ("Cargo.toml", "crates/rust-switcher-core/Cargo.toml")]
        versions += [value[1:] for value in git("tag", "--list", "v*").splitlines()
                     if re.fullmatch(r"v\d+\.\d+\.\d+", value)]
        base = max(tuple(map(int, checked_version(value).split("."))) for value in versions)
        version = checked_version(args.version) if args.version else f"{base[0]}.{base[1]}.{base[2] + 1}"
        if f"v{version}" in git("tag", "--list", f"v{version}").splitlines():
            raise ValueError("Version tag exists for another source")
        merged = ""
    directory = Path(args.directory)
    directory.mkdir(parents=True, exist_ok=False)
    plan = {"source": source, "version": version, "merged": merged}
    (directory / "plan.json").write_text(json.dumps(plan), encoding="utf-8")
    if not merged:
        for name, text in version_files(original, version).items():
            path = directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8", newline="\n")
    output(version=version, source=source)


def validate_plan(directory, source):
    directory = Path(directory)
    plan = json.loads((directory / "plan.json").read_text(encoding="utf-8"))
    if set(plan) != {"source", "version", "merged"} or plan["source"] != checked_sha(source):
        raise ValueError("Version plan identity mismatch")
    checked_version(plan["version"])
    expected = {"plan.json"}
    if plan["merged"]:
        checked_sha(plan["merged"])
        tag = existing_source_tag(source)
        if tag != "v" + plan["version"] or git("rev-parse", f"{tag}^{{commit}}") != plan["merged"]:
            raise ValueError("Retry tag identity changed")
    else:
        expected |= set(VERSION_FILES)
        for name, text in version_files(source_files(source), plan["version"]).items():
            if (directory / name).read_text(encoding="utf-8") != text:
                raise ValueError("Version artifact contains unrelated changes")
    actual = {path.relative_to(directory).as_posix() for path in directory.rglob("*") if path.is_file()}
    if actual != expected or any(path.is_symlink() for path in directory.rglob("*")):
        raise ValueError("Unexpected version artifact entries")
    return plan


def recover_pushed_branch(branch, source, version, proposed):
    """Recover push-success/PR-create-failure without replacing any remote branch."""
    ref = "refs/heads/" + branch
    helper = "credential.helper=!gh auth git-credential"
    advertised = git("-c", helper, "ls-remote", "--refs", "origin", ref)
    if not advertised:
        return None
    match = re.fullmatch(r"([a-f0-9]{40})\s+" + re.escape(ref), advertised)
    if not match:
        raise ValueError("Unexpected version branch advertisement")
    head = checked_sha(match[1])
    git("-c", helper, "fetch", "--no-tags", "origin", head)
    if git("rev-list", "--parents", "-n", "1", head).split() != [head, source]:
        raise ValueError("Existing version branch has an unexpected parent")
    intended = f"chore: release {version}\n\nrelease-source: {source}"
    if git("log", "-1", "--format=%B", head) != intended:
        raise ValueError("Existing version branch has an unexpected release identity")
    git("diff", "--exit-code", proposed, head)
    return head


def version_pr(args):
    plan = validate_plan(args.directory, args.source)
    if plan["merged"]:
        output(merged=plan["merged"], pr="", head="", version=plan["version"])
        return
    if git("rev-parse", "HEAD") != plan["source"]:
        raise ValueError("Writer checkout changed")
    for name in VERSION_FILES:
        shutil.copyfile(Path(args.directory) / name, ROOT / name)
    git("config", "user.name", "github-actions[bot]")
    git("config", "user.email", "41898282+github-actions[bot]@users.noreply.github.com")
    git("add", "--", *VERSION_FILES)
    git("commit", "-m", f'chore: release {plan["version"]}\n\nrelease-source: {plan["source"]}')
    proposed = git("rev-parse", "HEAD")
    branch = "automation/release-" + plan["source"]
    repo = os.environ["GITHUB_REPOSITORY"]
    prs = gh("pr", "list", "--repo", repo, "--base", "dev", "--head", branch,
             "--state", "all", "--json", "number,headRefOid,state")
    if len(prs) > 1:
        raise ValueError("Multiple version PRs for one source")
    if prs:
        pr = prs[0]
        if pr["state"] == "CLOSED":
            raise ValueError("Version PR was closed; publication refused")
        head = checked_sha(pr["headRefOid"])
        git("fetch", "origin", head)
        git("diff", "--exit-code", proposed, head)
    else:
        existing = recover_pushed_branch(branch, plan["source"], plan["version"], proposed)
        if existing:
            proposed = existing
        else:
            # Command-scoped helper; no persisted checkout credentials.
            git("-c", "credential.helper=!gh auth git-credential", "push", "origin", f"HEAD:refs/heads/{branch}")
        with tempfile.TemporaryDirectory() as temp:
            body = Path(temp) / "body.txt"
            body.write_text(f'Release {plan["version"]} for tested source {plan["source"]}. '
                            'Native CI and security gates must pass before publication.', encoding="utf-8")
            command("gh", "pr", "create", "--repo", repo, "--base", "dev", "--head", branch,
                    "--title", f'chore: release {plan["version"]}', "--body-file", str(body))
        pr = gh("pr", "view", branch, "--repo", repo, "--json", "number,headRefOid,state")
        head = checked_sha(pr["headRefOid"])
        if head != proposed:
            raise ValueError("Version PR head changed")
    if pr["state"] != "MERGED":
        command("gh", "pr", "merge", str(pr["number"]), "--repo", repo, "--auto", "--squash",
                "--match-head-commit", head, "--subject", f'chore: release {plan["version"]}',
                "--body", f'release-source: {plan["source"]}')
    output(merged="", pr=pr["number"], head=head, version=plan["version"])


def await_version(args):
    if args.commit:
        merged = checked_sha(args.commit)
    else:
        from release_gate import wait_for_merge
        merged = wait_for_merge(os.environ["GITHUB_REPOSITORY"], int(args.pr),
                                checked_sha(args.head), "dev")
    output(merged=merged)


def expected_files(version):
    checked_version(version)
    return {"rust-switcher.exe", f"rust-switcher-{version}-x86_64-pc-windows-msvc.zip",
            *(f"{name}-{version}.crate" for name in PACKAGES)}


def make_bundle(args):
    source, commit = checked_sha(args.source), checked_sha(args.commit)
    version = checked_version(args.version)
    if git("rev-parse", "HEAD") != commit:
        raise ValueError("Build checkout changed")
    git("diff", "--exit-code", "HEAD")
    for name in ("Cargo.toml", "crates/rust-switcher-core/Cargo.toml"):
        if tomllib.loads((ROOT / name).read_text(encoding="utf-8"))["package"]["version"] != version:
            raise ValueError("Build package version differs from checked version")
    directory = Path(args.directory)
    directory.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(ROOT / "target/release/rust-switcher.exe", directory / "rust-switcher.exe")
    with zipfile.ZipFile(directory / f"rust-switcher-{version}-x86_64-pc-windows-msvc.zip", "w") as archive:
        info = zipfile.ZipInfo("rust-switcher.exe", date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        archive.writestr(info, (directory / "rust-switcher.exe").read_bytes())
    for name in PACKAGES:
        filename = f"{name}-{version}.crate"
        shutil.copyfile(ROOT / "target/package" / filename, directory / filename)
    pdb = ROOT / "target/release/rust-switcher.pdb"
    if pdb.exists():
        shutil.copyfile(pdb, directory / pdb.name)
    manifest = {"source": source, "commit": commit, "version": version,
                "rustc": command("rustc", "+nightly", "-vV"),
                "cargo": command("cargo", "+nightly", "-V"),
                "files": {path.name: {"sha256": digest(path), "size": path.stat().st_size}
                          for path in directory.iterdir()}}
    path = directory / "manifest.json"
    path.write_text(json.dumps(manifest, sort_keys=True), encoding="utf-8")
    output(manifest_sha=digest(path))


def validate_bundle(directory, source, commit, version, manifest_sha):
    directory = Path(directory)
    if not re.fullmatch(r"[a-f0-9]{64}", manifest_sha) or digest(directory / "manifest.json") != manifest_sha:
        raise ValueError("Release manifest digest mismatch")
    manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    if (manifest["source"], manifest["commit"], manifest["version"]) != (
            checked_sha(source), checked_sha(commit), checked_version(version)):
        raise ValueError("Release manifest identity mismatch")
    allowed = expected_files(version)
    actual = set(manifest["files"])
    if actual not in (allowed, allowed | {"rust-switcher.pdb"}):
        raise ValueError("Unexpected publication filenames")
    if {path.name for path in directory.iterdir()} != actual | {"manifest.json"}:
        raise ValueError("Unexpected publication directory entries")
    for name, record in manifest["files"].items():
        path = directory / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size != record["size"] or digest(path) != record["sha256"]:
            raise ValueError("Publication file integrity mismatch: " + name)
    for package in PACKAGES:
        with tarfile.open(directory / f"{package}-{version}.crate", "r:gz") as archive:
            prefix = f"{package}-{version}/"
            for member in archive.getmembers():
                if (not (member.name.startswith(prefix) or member.name == prefix.rstrip("/"))
                        or ".." in Path(member.name).parts or not (member.isfile() or member.isdir())):
                    raise ValueError("Unsafe crate archive member")
            vcs = json.load(archive.extractfile(prefix + ".cargo_vcs_info.json"))
            if vcs["git"]["sha1"] != commit or vcs["git"].get("dirty", False):
                raise ValueError("Crate source identity mismatch")
            metadata = tomllib.loads(archive.extractfile(prefix + "Cargo.toml").read().decode("utf-8"))
            if (metadata["package"]["name"], metadata["package"]["version"]) != (package, version):
                raise ValueError("Crate package identity mismatch")
    return manifest


def cargo_environment(temporary, token=None):
    # No repository Cargo config, wrappers, credential providers, caches, or build outputs.
    allowed = {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP", "USERPROFILE",
               "HOME", "RUSTUP_HOME", "PROCESSOR_ARCHITECTURE", "NUMBER_OF_PROCESSORS"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env["CARGO_HOME"] = str(temporary / "cargo-home")
    env["CARGO_TARGET_DIR"] = str(temporary / "target")
    env["CARGO_TERM_COLOR"] = "never"
    if token:
        env["CARGO_REGISTRY_TOKEN"] = token
    return env


def preflight(args):
    manifest = validate_bundle(args.directory, args.source, args.commit, args.version, args.manifest_sha)
    if git("rev-parse", "HEAD") != args.commit:
        raise ValueError("Publisher checkout mismatch")
    git("diff", "--exit-code")
    temporary = Path(args.sterile)
    temporary.mkdir(parents=True, exist_ok=False)
    env = cargo_environment(temporary)
    cargo = shutil.which("cargo")
    if not cargo:
        raise ValueError("Cargo unavailable")
    if command(cargo, "+nightly", "-V", cwd=temporary, env=env) != manifest["cargo"]:
        raise ValueError("Publisher Cargo version changed since verification")
    if command("rustc", "+nightly", "-vV", cwd=temporary, env=env) != manifest["rustc"]:
        raise ValueError("Publisher Rust version changed since verification")
    command(cargo, "+nightly", "package", "--workspace", "--locked", "--no-verify",
            "--manifest-path", str(ROOT / "Cargo.toml"), cwd=temporary, env=env)
    for package in PACKAGES:
        filename = f"{package}-{args.version}.crate"
        if digest(temporary / "target/package" / filename) != manifest["files"][filename]["sha256"]:
            raise ValueError("Repackaged crate differs from unprivileged verified package")


def publish_github(args):
    validate_bundle(args.directory, args.source, args.commit, args.version, args.manifest_sha)
    repo = os.environ["GITHUB_REPOSITORY"]
    tag = "v" + args.version
    tags = gh("api", f"repos/{repo}/git/matching-refs/tags/{tag}")
    exact = [ref for ref in tags if ref["ref"] == "refs/tags/" + tag]
    if exact:
        # Existing supported flow creates lightweight tags. Never move another tag.
        if len(exact) != 1 or exact[0]["object"]["type"] != "commit" or exact[0]["object"]["sha"] != args.commit:
            raise ValueError("Immutable release tag mismatch")
    else:
        command("gh", "api", "-X", "POST", f"repos/{repo}/git/refs", "-f",
                "ref=refs/tags/" + tag, "-f", "sha=" + args.commit)
    releases = gh("release", "list", "--repo", repo, "--limit", "1000", "--json", "tagName")
    if not any(release["tagName"] == tag for release in releases):
        with tempfile.TemporaryDirectory() as temporary:
            notes = Path(temporary) / "notes.txt"
            notes.write_text(f"Release {tag}\n\nCommit: {args.commit}\nSource: {args.source}\n", encoding="utf-8")
            command("gh", "release", "create", tag, "--repo", repo, "--title", tag,
                    "--notes-file", str(notes), "--verify-tag")
    assets = [str(path) for path in Path(args.directory).iterdir() if path.suffix in (".exe", ".zip", ".pdb")]
    # Preserve existing same-tag retry behavior; the tag itself is never moved.
    command("gh", "release", "upload", tag, "--repo", repo, *sorted(assets), "--clobber")


def index_checksum(package, version):
    url = f"https://index.crates.io/{package[:2]}/{package[2:4]}/{package}"
    request = urllib.request.Request(url, headers={"User-Agent": "rust-switcher-release"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            records = response.read().decode("utf-8").splitlines()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise
    for line in records:
        record = json.loads(line)
        if record["vers"] == version:
            if record.get("yanked"):
                raise ValueError("Refusing to treat a yanked version as publication success")
            return record["cksum"]
    return None


def publish_crates(args):
    manifest = validate_bundle(args.directory, args.source, args.commit, args.version, args.manifest_sha)
    if git("rev-parse", "HEAD") != args.commit:
        raise ValueError("Publisher checkout mismatch")
    git("diff", "--exit-code")
    temporary = Path(args.sterile)
    if not temporary.is_dir():
        raise ValueError("Missing tokenless publication preflight")
    token = os.environ.get("CARGO_REGISTRY_TOKEN")
    if not token:
        raise ValueError("No crates.io token")
    cargo = shutil.which("cargo")
    if not cargo:
        raise ValueError("Cargo unavailable")
    env = cargo_environment(temporary, token)
    for package in PACKAGES:
        filename = f"{package}-{args.version}.crate"
        expected = manifest["files"][filename]["sha256"]
        if digest(temporary / "target/package" / filename) != expected:
            raise ValueError("Preflight package changed")
        existing = index_checksum(package, args.version)
        if existing is not None:
            if existing != expected:
                raise ValueError("Published version has another checksum")
            continue
        publish_error = None
        for attempt in range(20):
            try:
                # Reproduce individual publish input without a token before every attempt.
                command(cargo, "+nightly", "package", "--locked", "--no-verify", "--registry", "crates-io",
                        "--manifest-path", str(ROOT / "Cargo.toml"), "-p", package,
                        cwd=temporary, env=cargo_environment(temporary))
                if digest(temporary / "target/package" / filename) != expected:
                    raise ValueError("Individual package differs from the verified release archive")
                command(cargo, "+nightly", "publish", "--locked", "--no-verify", "--registry", "crates-io",
                        "--manifest-path", str(ROOT / "Cargo.toml"), "-p", package, cwd=temporary, env=env)
                break
            except RuntimeError as error:
                publish_error = error
                dependency_pending = package == "rust-switcher" and any(
                    message in str(error) for message in (
                        "failed to select a version for the requirement", "no matching package named",
                        "could not find", "rust-switcher-core not found"))
                if dependency_pending and attempt < 19 and index_checksum(package, args.version) is None:
                    time.sleep(15)
                    continue
                break
        # An upload can succeed before the response is lost. Reconcile by checksum.
        for _ in range(20):
            existing = index_checksum(package, args.version)
            if existing is not None:
                if existing != expected:
                    raise ValueError("Published package checksum mismatch")
                break
            time.sleep(15)
        else:
            raise RuntimeError("Published package is not confirmed in the index") from publish_error


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("stage", choices=("prepare", "version-pr", "await-version", "bundle", "preflight", "publish-github", "publish-crates"))
    for name in ("source", "commit", "version", "directory", "manifest-sha", "sterile", "pr", "head"):
        parser.add_argument("--" + name, default="")
    args = parser.parse_args()
    {"prepare": prepare, "version-pr": version_pr, "await-version": await_version,
     "bundle": make_bundle, "preflight": preflight, "publish-github": publish_github,
     "publish-crates": publish_crates}[args.stage](args)


if __name__ == "__main__":
    main()
