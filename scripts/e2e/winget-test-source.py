#!/usr/bin/env python3
"""HTTPS REST source for the real WinGet acceptance test.

Requires PyYAML==6.0.3. Read validated, production-generated multifile YAML
manifests; this adapter does not generate installer identities or payload hashes.
The caller owns certificate trust and WinGet source registration/cleanup. Keep
certificate/private-key files outside any uploaded diagnostic directory.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import ssl
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlsplit

import yaml

CONTRACT = "1.28.0"
MAX_BODY = 65536
MAX_DEPENDENCIES = 8
MAX_DEPENDENCY_BYTES = 256 * 1024 * 1024
DOCUMENT_KEYS = {"PackageIdentifier", "PackageVersion", "ManifestType", "ManifestVersion"}


class FixtureError(ValueError):
    """Invalid fixture data; never include document contents in public errors."""


class ManifestLoader(yaml.SafeLoader):
    def construct_mapping(self, node, deep=False):
        mapping = super().construct_mapping(node, deep=deep)
        # WinGet reads these schema string fields as scalar text. In particular,
        # an unquoted SHA256 containing only digits must not become an integer
        # or lose leading zeroes through PyYAML's implicit numeric resolver.
        string_fields = {"InstallerSha256", "SignatureSha256", "PackageVersion", "ManifestVersion",
                         "ProductCode", "UpgradeCode", "DisplayVersion", "MinimumOSVersion", "MinimumVersion"}
        for key, value in node.value:
            if key.value in string_fields and isinstance(value, yaml.ScalarNode):
                mapping[key.value] = value.value
        return mapping


# YAML dates are strings in the REST schema; YAML 1.2 also leaves 'on'/'off'
# as strings. Copy resolver tables so importing this helper cannot affect users
# of PyYAML's normal SafeLoader in the same process.
ManifestLoader.yaml_implicit_resolvers = {
    key: [(tag, rx) for tag, rx in entries if tag not in {
        "tag:yaml.org,2002:timestamp", "tag:yaml.org,2002:bool"
    }]
    for key, entries in yaml.SafeLoader.yaml_implicit_resolvers.items()
}
ManifestLoader.add_implicit_resolver(
    "tag:yaml.org,2002:bool", re.compile(r"^(?:true|false|True|False|TRUE|FALSE)$"), list("tTfF")
)


def required_string(obj: dict, key: str) -> str:
    value = obj.get(key)
    if not isinstance(value, str) or not value:
        raise FixtureError(f"Missing string field: {key}")
    return value


def object_without(obj: dict, excluded: set[str]) -> dict:
    return {key: copy.deepcopy(value) for key, value in obj.items() if key not in excluded}


def load_catalog(root: Path) -> dict:
    """Load an atomic snapshot; partially published versions fail closed."""
    documents: dict[Path, dict[str, dict]] = {}
    files = sorted(root.rglob("*.yaml"))
    if not files or len(files) > 300:
        raise FixtureError("Expected a bounded collection of manifest files")
    for path in files:
        if not path.resolve().is_relative_to(root.resolve()) or path.stat().st_size > 1048576:
            raise FixtureError("Manifest path or size is invalid")
        try:
            obj = yaml.load(path.read_text(encoding="utf-8-sig"), Loader=ManifestLoader)
        except (yaml.YAMLError, UnicodeError) as error:
            raise FixtureError("Cannot parse a manifest") from error
        if not isinstance(obj, dict):
            raise FixtureError("Manifest must be an object")
        kind = required_string(obj, "ManifestType")
        if kind not in {"version", "installer", "defaultLocale"}:
            raise FixtureError("Only the three generated manifest kinds are supported")
        group = documents.setdefault(path.parent, {})
        if kind in group:
            raise FixtureError("Duplicate manifest kind in a version directory")
        group[kind] = obj

    package_id = None
    versions = []
    identities = set()
    for group in documents.values():
        if set(group) != {"version", "installer", "defaultLocale"}:
            raise FixtureError("Incomplete production manifest set")
        version_doc, installer_doc, locale_doc = (group[k] for k in ("version", "installer", "defaultLocale"))
        identity = {(required_string(doc, "PackageIdentifier"), required_string(doc, "PackageVersion"))
                    for doc in group.values()}
        if len(identity) != 1:
            raise FixtureError("Manifest identities disagree")
        current_id, version = identity.pop()
        if package_id is not None and current_id != package_id:
            raise FixtureError("A fixture source must contain exactly one package")
        package_id = current_id
        channel = version_doc.get("Channel", "")
        if not isinstance(channel, str) or any(doc.get("Channel", channel) != channel for doc in group.values()):
            raise FixtureError("Manifest channels disagree")
        if (version, channel) in identities:
            raise FixtureError("Duplicate package version/channel")
        identities.add((version, channel))
        if required_string(version_doc, "DefaultLocale") != required_string(locale_doc, "PackageLocale"):
            raise FixtureError("Default locale does not match its locale document")
        for key in ("PackageName", "Publisher", "ShortDescription"):
            required_string(locale_doc, key)
        raw_installers = installer_doc.get("Installers")
        if not isinstance(raw_installers, list) or not raw_installers:
            raise FixtureError("Missing installers")
        defaults = object_without(installer_doc, DOCUMENT_KEYS | {"Installers", "Channel"})
        installers = []
        for raw in raw_installers:
            if not isinstance(raw, dict):
                raise FixtureError("Installer must be an object")
            installer = copy.deepcopy(defaults)
            # WinGet inheritance replaces an explicitly supplied field; it does
            # not concatenate arrays such as AppsAndFeaturesEntries.
            installer.update(copy.deepcopy(raw))
            for key in ("Architecture", "InstallerType", "InstallerUrl", "InstallerSha256"):
                required_string(installer, key)
            if not re.fullmatch(r"[0-9a-fA-F]{64}", installer["InstallerSha256"]):
                raise FixtureError("Invalid installer digest")
            if not isinstance(installer.get("AppsAndFeaturesEntries", []), list):
                raise FixtureError("Invalid AppsAndFeaturesEntries")
            installers.append(installer)
        versions.append({
            "PackageVersion": version, "Channel": channel,
            "DefaultLocale": object_without(locale_doc, DOCUMENT_KEYS | {"Channel"}),
            "Installers": installers,
        })
    catalog = {"PackageIdentifier": package_id, "Versions": versions}
    try:
        json.dumps(catalog, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise FixtureError("Manifest has a value unsupported by JSON") from error
    return catalog


def product_codes(version: dict) -> list[str]:
    codes = set()
    for installer in version["Installers"]:
        if installer.get("ProductCode"):
            codes.add(required_string(installer, "ProductCode"))
        for entry in installer.get("AppsAndFeaturesEntries", []):
            if not isinstance(entry, dict):
                raise FixtureError("AppsAndFeaturesEntries item must be an object")
            if entry.get("ProductCode"):
                codes.add(required_string(entry, "ProductCode"))
    return sorted(codes)


def declared_dependencies(catalog: dict) -> list[dict]:
    """Derive the allowlist from the unchanged selected production installers."""
    dependencies = {}
    for version in catalog["Versions"]:
        for installer in version["Installers"]:
            fields = installer.get("Dependencies", {})
            if not isinstance(fields, dict):
                raise FixtureError("Invalid dependency declaration")
            packages = fields.get("PackageDependencies", [])
            if not isinstance(packages, list):
                raise FixtureError("Invalid package dependency declaration")
            for item in packages:
                if not isinstance(item, dict) or set(item) - {"PackageIdentifier", "MinimumVersion"}:
                    raise FixtureError("Invalid package dependency fields")
                package = required_string(item, "PackageIdentifier")
                if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.+-]{1,127}", package):
                    raise FixtureError("Invalid dependency identifier")
                key = package.casefold()
                if key == catalog["PackageIdentifier"].casefold():
                    raise FixtureError("A package cannot depend on itself")
                value = dependencies.setdefault(key, {"PackageIdentifier": package, "MinimumVersions": []})
                minimum = item.get("MinimumVersion")
                if minimum is not None:
                    if not isinstance(minimum, str) or not minimum or len(minimum) > 128:
                        raise FixtureError("Invalid dependency minimum version")
                    if minimum not in value["MinimumVersions"]:
                        value["MinimumVersions"].append(minimum)
                if len(dependencies) > MAX_DEPENDENCIES:
                    raise FixtureError("Too many fixture dependencies")
    return [dependencies[key] for key in sorted(dependencies)]


def load_dependency_catalogs(root: Path | None, primary: dict, *, verify_downloads: bool = False) -> tuple[dict, list[dict]]:
    """Adapt only official `winget download` merged exports, without rewriting fields.

    WinGet 1.29.380 exports one selected installer plus root default-locale
    fields and optional Localization. Its original URL, hash, scope, product
    codes, upgrade codes, switches, and dependencies all survive this mapping.
    """
    declared = declared_dependencies(primary)
    files = sorted(root.rglob("*.yaml")) if root is not None else []
    if len(files) > MAX_DEPENDENCIES:
        raise FixtureError("Too many exported dependency manifests")
    catalogs, evidence = {}, []
    total_bytes = 0
    for path in files:
        if not path.resolve().is_relative_to(root.resolve()) or path.stat().st_size > 1048576:
            raise FixtureError("Exported dependency path or size is invalid")
        try:
            doc = yaml.load(path.read_text(encoding="utf-8-sig"), Loader=ManifestLoader)
        except (yaml.YAMLError, UnicodeError) as error:
            raise FixtureError("Cannot parse an exported dependency") from error
        if not isinstance(doc, dict) or doc.get("ManifestType") != "merged":
            raise FixtureError("Expected a WinGet merged dependency export")
        package = required_string(doc, "PackageIdentifier")
        version = required_string(doc, "PackageVersion")
        required_string(doc, "ManifestVersion")
        if package.casefold() in catalogs or package.casefold() == primary["PackageIdentifier"].casefold():
            raise FixtureError("Duplicate or primary-package dependency export")
        installers = doc.get("Installers")
        if not isinstance(installers, list) or len(installers) != 1 or not isinstance(installers[0], dict):
            raise FixtureError("Expected one exported selected dependency installer")
        installer = installers[0]
        for key in ("Architecture", "InstallerType", "InstallerUrl", "InstallerSha256"):
            required_string(installer, key)
        digest = installer["InstallerSha256"]
        url = urlsplit(installer["InstallerUrl"])
        if not re.fullmatch(r"[0-9a-fA-F]{64}", digest) or url.scheme != "https" or not url.hostname or url.username or url.password:
            raise FixtureError("Invalid exported dependency URL or digest")
        locale = object_without(doc, DOCUMENT_KEYS | {"Channel", "Installers", "Localization"})
        for key in ("PackageLocale", "PackageName", "Publisher", "ShortDescription"):
            required_string(locale, key)
        current = {"PackageVersion": version, "Channel": doc.get("Channel", ""),
                   "DefaultLocale": locale, "Installers": copy.deepcopy(installers)}
        if not isinstance(current["Channel"], str):
            raise FixtureError("Invalid exported dependency channel")
        if "Localization" in doc:
            if not isinstance(doc["Localization"], list) or not all(isinstance(item, dict) for item in doc["Localization"]):
                raise FixtureError("Invalid exported dependency localizations")
            current["Locales"] = copy.deepcopy(doc["Localization"])
        catalog = {"PackageIdentifier": package, "Versions": [current]}
        catalogs[package.casefold()] = catalog
        record = {"PackageIdentifier": package, "PackageVersion": version,
                  "InstallerUrl": installer["InstallerUrl"], "InstallerSha256": digest,
                  "Architecture": installer["Architecture"], "Scope": installer.get("Scope", ""),
                  "ProductCodes": product_codes(current), "Manifest": str(path)}
        if verify_downloads:
            payloads = [candidate for candidate in path.parent.iterdir()
                        if candidate.is_file() and candidate.stem == path.stem and candidate.suffix != ".yaml"]
            if len(payloads) != 1 or not payloads[0].resolve().is_relative_to(root.resolve()):
                raise FixtureError("Expected the matching WinGet-downloaded dependency payload")
            payload = payloads[0]
            size = payload.stat().st_size
            total_bytes += size
            if size <= 0 or size > 128 * 1024 * 1024 or total_bytes > MAX_DEPENDENCY_BYTES:
                raise FixtureError("Dependency payload exceeds the fixture size limit")
            hasher = hashlib.sha256()
            with payload.open("rb") as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                    hasher.update(chunk)
            if hasher.hexdigest().casefold() != digest.casefold():
                raise FixtureError("Official downloaded dependency hash does not match its exported manifest")
            record.update({"DownloadedBytes": size, "DownloadedSha256": hasher.hexdigest(), "Payload": str(payload)})
        evidence.append(record)

    # Only declared IDs, including an explicitly declared transitive dependency,
    # can enter this fixture. WinGet itself remains the minimum-version arbiter.
    allowed = {item["PackageIdentifier"].casefold() for item in declared}
    visited = set()
    while allowed - visited:
        if len(allowed) > MAX_DEPENDENCIES:
            raise FixtureError("Too many transitive fixture dependencies")
        package = next(iter(allowed - visited))
        if package not in catalogs:
            raise FixtureError("A declared dependency has no official exported manifest")
        visited.add(package)
        allowed.update(item["PackageIdentifier"].casefold() for item in declared_dependencies(catalogs[package]))
    if set(catalogs) != allowed:
        raise FixtureError("An exported dependency was not declared by the production manifests")
    try:
        json.dumps(catalogs, allow_nan=False)
    except (TypeError, ValueError) as error:
        raise FixtureError("Dependency manifest has a value unsupported by JSON") from error
    return catalogs, evidence


def inspect_dependencies(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Inspect production dependencies and verified WinGet exports")
    parser.add_argument("--manifest-root", type=Path, required=True)
    parser.add_argument("--dependency-root", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        primary = load_catalog(args.manifest_root)
        result = {"PackageIdentifier": primary["PackageIdentifier"], "Dependencies": declared_dependencies(primary)}
        if args.dependency_root is not None:
            _, result["Exports"] = load_dependency_catalogs(args.dependency_root, primary, verify_downloads=True)
        with args.output.open("x", encoding="utf-8") as stream:
            json.dump(result, stream, allow_nan=False)
        return 0
    except (FixtureError, OSError, ValueError) as error:
        print(f"Dependency fixture validation failed: {error}", file=sys.stderr)
        return 1


def field_values(catalog: dict, field: str) -> list[str]:
    versions = catalog["Versions"]
    if field == "PackageIdentifier":
        return [catalog["PackageIdentifier"]]
    if field == "ProductCode":
        return [code for version in versions for code in product_codes(version)]
    if field in {"PackageName", "Moniker"}:
        return [version["DefaultLocale"][field] for version in versions if field in version["DefaultLocale"]]
    if field == "Tag":
        return [tag for version in versions for tag in version["DefaultLocale"].get("Tags", [])]
    if field in {"Command", "PackageFamilyName"}:
        key = "Commands" if field == "Command" else "PackageFamilyName"
        values = [installer.get(key, []) for version in versions for installer in version["Installers"]]
        return [item for value in values for item in (value if isinstance(value, list) else [value])]
    if field == "NormalizedPackageNameAndPublisher":
        # Correlation here deliberately requires the real product code. The
        # fixture does not emulate WinGet's native name/publisher normalizer.
        return []
    raise FixtureError("Unsupported search field")


def match_values(values: list[str], request: dict) -> bool:
    if not isinstance(request, dict) or set(request) != {"KeyWord", "MatchType"}:
        raise FixtureError("Invalid request match")
    keyword = request.get("KeyWord")
    if not isinstance(keyword, str):
        raise FixtureError("Invalid search keyword")
    kind = required_string(request, "MatchType")
    if kind not in {"Exact", "CaseInsensitive", "StartsWith", "Substring"}:
        raise FixtureError("Unsupported search match type")
    if not keyword:
        return False
    if kind == "Exact":
        return keyword in values
    if kind == "CaseInsensitive":
        return any(keyword.casefold() == value.casefold() for value in values)
    if kind == "StartsWith":
        return any(value.casefold().startswith(keyword.casefold()) for value in values)
    if kind == "Substring":
        return any(keyword.casefold() in value.casefold() for value in values)
    raise FixtureError("Unsupported search match type")


def search_catalog(catalog: dict, request: dict) -> dict:
    if not isinstance(request, dict) or set(request) - {"Query", "Filters", "Inclusions", "MaximumResults", "FetchAllManifests"}:
        raise FixtureError("Invalid search request")
    maximum = request.get("MaximumResults", 0)
    if type(maximum) is not int or maximum < 0:
        raise FixtureError("Invalid result limit")
    if "FetchAllManifests" in request and type(request["FetchAllManifests"]) is not bool:
        raise FixtureError("Invalid FetchAllManifests")
    filters, inclusions = request.get("Filters", []), request.get("Inclusions", [])
    if not isinstance(filters, list) or not isinstance(inclusions, list):
        raise FixtureError("Search filters must be arrays")

    def match_filter(item: dict) -> bool:
        if not isinstance(item, dict) or set(item) != {"PackageMatchField", "RequestMatch"}:
            raise FixtureError("Invalid package filter")
        return match_values(field_values(catalog, required_string(item, "PackageMatchField")), item["RequestMatch"])

    # Evaluate every predicate, even if an earlier predicate matched, so invalid
    # filters cannot be ignored through boolean short-circuiting.
    filtered = [match_filter(item) for item in filters]
    included = [match_filter(item) for item in inclusions]
    if "Query" in request:
        values = [value for field in ("PackageIdentifier", "PackageName", "Moniker", "Tag", "Command")
                  for value in field_values(catalog, field)]
        included.append(match_values(values, request["Query"]))
    fetch_all = request.get("FetchAllManifests", False)
    if fetch_all and (filters or inclusions or "Query" in request):
        raise FixtureError("FetchAllManifests cannot be combined with predicates")
    if not fetch_all and not (filters or inclusions or "Query" in request):
        raise FixtureError("Empty search must explicitly request all manifests")
    matched = all(filtered) and (any(included) if included else True)
    if not matched:
        return {"Data": []}
    locale = catalog["Versions"][0]["DefaultLocale"]
    return {"Data": [{
        "PackageIdentifier": catalog["PackageIdentifier"],
        "PackageName": locale["PackageName"], "Publisher": locale["Publisher"],
        "Versions": [{"PackageVersion": version["PackageVersion"], "Channel": version["Channel"],
                      "ProductCodes": product_codes(version)} for version in catalog["Versions"]],
    }]}


def search_catalogs(catalogs: dict, request: dict) -> dict:
    matches = [match for catalog in catalogs.values() for match in search_catalog(catalog, request)["Data"]]
    maximum = request.get("MaximumResults", 0)
    return {"Data": matches[:maximum] if maximum else matches}


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False
    allow_reuse_address = False

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(3)
        # Defer handshakes to the bounded worker, so an idle TLS client cannot
        # prevent the main loop from observing the stop file.
        try:
            return self.tls.wrap_socket(connection, server_side=True, do_handshake_on_connect=False), address
        except BaseException:
            connection.close()
            raise

    def handle_error(self, _request, _address):
        self.event("connection-error")

    def event(self, name: str, **fields):
        with self.log_lock:
            if self.log.closed:
                return
            self.log.write(json.dumps({"event": name, **fields}, separators=(",", ":")) + "\n")
            self.log.flush()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, _format, *_args):
        pass  # No headers, raw URLs, bodies, certificate paths or key material.

    def respond(self, status: int, body: dict, route: str):
        data = json.dumps(body, allow_nan=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        self.wfile.write(data)
        self.server.event("request", method=self.command, route=route, status=status)

    def do_GET(self):
        self.dispatch()

    def do_POST(self):
        self.dispatch()

    def dispatch(self):
        route = "unknown"
        try:
            url = urlsplit(self.path)
            if url.scheme or url.netloc or url.fragment:
                raise FixtureError("Invalid request target")
            if self.command == "GET" and url.path == "/information" and not url.query:
                return self.respond(200, {"Data": {"SourceIdentifier": self.server.source_id,
                    "ServerSupportedVersions": [CONTRACT]}}, "information")
            if self.headers.get("Version") != CONTRACT:
                return self.respond(400, {"code": "UnsupportedContract", "message": "Use the advertised contract"}, route)
            try:
                catalog = load_catalog(self.server.manifest_root)
                if catalog["PackageIdentifier"] != self.server.package_id:
                    raise FixtureError("Source package identity changed")
                if declared_dependencies(catalog) != self.server.dependency_plan:
                    raise FixtureError("Production dependencies changed after source hydration")
                catalogs = {catalog["PackageIdentifier"].casefold(): catalog, **self.server.dependency_catalogs}
            except (FixtureError, OSError):
                return self.respond(503, {"code": "InvalidFixture", "message": "Manifest snapshot is unavailable"}, route)
            if self.command == "GET" and url.path.startswith("/packageManifests/"):
                route = "manifests"
                package_id = unquote(url.path[len("/packageManifests/"):])
                query = parse_qs(url.query, keep_blank_values=True, strict_parsing=True)
                if set(query) - {"Version", "Channel"} or any(len(value) != 1 for value in query.values()):
                    raise FixtureError("Invalid manifest query")
                selected = catalogs.get(package_id.casefold())
                versions = [version for version in selected["Versions"] if all(
                    version["PackageVersion" if key == "Version" else "Channel"] == value[0]
                    for key, value in query.items())] if selected else []
                if not versions:
                    return self.respond(404, {"code": "NotFound", "message": "No matching package version"}, route)
                return self.respond(200, {"Data": {"PackageIdentifier": selected["PackageIdentifier"], "Versions": versions}}, route)
            if self.command == "POST" and url.path == "/manifestSearch" and not url.query:
                route = "search"
                if self.headers.get("Transfer-Encoding") or self.headers.get_content_type() != "application/json":
                    raise FixtureError("Expected bounded JSON body")
                size = int(self.headers.get("Content-Length", "-1"))
                if not 0 < size <= MAX_BODY:
                    raise FixtureError("Invalid body length")
                data = self.rfile.read(size)
                if len(data) != size:
                    raise FixtureError("Incomplete body")
                return self.respond(200, search_catalogs(catalogs, json.loads(data)), route)
            return self.respond(404, {"code": "NotFound", "message": "Unknown endpoint"}, route)
        except (FixtureError, ValueError, UnicodeError):
            self.respond(400, {"code": "InvalidRequest", "message": "Unsupported fixture request"}, route)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest-root", type=Path, required=True)
    parser.add_argument("--dependency-root", type=Path)
    parser.add_argument("--bind", choices=["127.0.0.1"], default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--cert-file", type=Path, required=True)
    parser.add_argument("--key-file", type=Path, required=True)
    parser.add_argument("--source-id", required=True)
    parser.add_argument("--ready-file", type=Path, required=True)
    parser.add_argument("--stop-file", type=Path, required=True)
    parser.add_argument("--log-file", type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", args.source_id) or not 0 <= args.port <= 65535:
        parser.error("Invalid source identifier or port")
    if args.ready_file.exists() or args.stop_file.exists():
        parser.error("Readiness and stop paths must be new")
    ready_written = False
    try:
        with args.log_file.open("x", encoding="utf-8") as log:
            try:
                catalog = load_catalog(args.manifest_root)
                dependencies, _ = load_dependency_catalogs(args.dependency_root, catalog)
                tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                tls.minimum_version = ssl.TLSVersion.TLSv1_2
                tls.load_cert_chain(args.cert_file, args.key_file)
                stopping = threading.Event()
                for signum in (signal.SIGINT, signal.SIGTERM):
                    signal.signal(signum, lambda _signum, _frame: stopping.set())
                with FixtureServer((args.bind, args.port), Handler) as server:
                    server.timeout, server.tls = 0.2, tls
                    server.log, server.log_lock = log, threading.Lock()
                    server.source_id, server.manifest_root = args.source_id, args.manifest_root
                    server.package_id = catalog["PackageIdentifier"]
                    server.dependency_plan = declared_dependencies(catalog)
                    server.dependency_catalogs = dependencies
                    ready = {"baseUrl": f"https://localhost:{server.server_port}", "port": server.server_port,
                             "pid": os.getpid(), "sourceIdentifier": args.source_id, "contractVersion": CONTRACT,
                             "packageIdentifier": server.package_id,
                             "dependencyIdentifiers": sorted(value["PackageIdentifier"] for value in dependencies.values())}
                    temporary = args.ready_file.with_name(args.ready_file.name + ".tmp")
                    temporary.write_text(json.dumps(ready), encoding="utf-8")
                    temporary.replace(args.ready_file)
                    ready_written = True
                    server.event("ready", port=server.server_port, package=server.package_id, versions=len(catalog["Versions"]),
                                 dependencies=ready["dependencyIdentifiers"])
                    while not stopping.is_set() and not args.stop_file.exists():
                        server.handle_request()
                    server.event("stopped")
            except (FixtureError, OSError, ValueError) as error:
                entry = {"event": "fixture-failed", "errorType": type(error).__name__}
                if isinstance(error, FixtureError):
                    entry["reason"] = str(error)
                log.write(json.dumps(entry) + "\n")
                log.flush()
                raise
        return 0
    except (FixtureError, OSError, ValueError):
        print("REST fixture startup or runtime failed; check the validated manifest set and TLS inputs.", file=sys.stderr)
        return 1
    finally:
        if ready_written:
            args.ready_file.unlink(missing_ok=True)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "inspect-dependencies":
        raise SystemExit(inspect_dependencies(sys.argv[2:]))
    raise SystemExit(main())
