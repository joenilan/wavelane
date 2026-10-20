#!/usr/bin/env python3
"""Collect license metadata and shipped license texts for active Linux deps."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "Cargo.toml"
PLATFORM = "x86_64-unknown-linux-gnu"
MAX_TEXT = 1024 * 1024
LICENSE_NAMES = ("LICENSE*", "LICENCE*", "UNLICENSE*", "COPYING*", "NOTICE*")
MAX_SUPPLEMENTAL = 32 * 1024 * 1024
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


def safe_text(path: Path, crate_root: Path) -> tuple[str | None, str | None]:
    try:
        if ".." in path.relative_to(crate_root).parts:
            return None, "parent traversal in license path"
        resolved = path.resolve(strict=True)
        resolved.relative_to(crate_root)
        cursor = path
        while cursor != crate_root:
            if cursor.is_symlink():
                return None, "symlink in license path"
            cursor = cursor.parent
        info = resolved.stat()
        if not resolved.is_file() or info.st_size > MAX_TEXT:
            return None, "not a regular file or exceeds 1 MiB"
        raw = resolved.read_bytes()
        if b"\0" in raw:
            return None, "binary content"
        return raw.decode("utf-8", errors="strict"), None
    except (OSError, UnicodeError, ValueError) as error:
        return None, str(error)


def read_vcs_commit(crate_root: Path) -> str | None:
    """Read the commit recorded in a packaged crate, without invoking Cargo."""
    path = crate_root / ".cargo_vcs_info.json"
    try:
        if path.is_symlink() or path.stat().st_size > 64 * 1024:
            return None
        data = json.loads(path.read_text(encoding="utf-8"))
        commit = data.get("git", {}).get("sha1")
        return commit if isinstance(commit, str) and COMMIT_RE.fullmatch(commit) else None
    except (OSError, UnicodeError, json.JSONDecodeError, AttributeError):
        return None


def load_supplemental(path: Path) -> dict:
    """Load and validate the pinned, offline supplemental-notices artifact."""
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or resolved.stat().st_size > MAX_SUPPLEMENTAL:
        raise ValueError("supplemental artifact is not a regular file or exceeds 32 MiB")
    data = json.loads(resolved.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError("supplemental artifact must be a JSON object")
    provenance = data.get("provenance")
    if (not isinstance(provenance, str) or
            ".cargo_vcs_info.json" not in provenance or
            "exact GitHub commits" not in provenance):
        raise ValueError("supplemental artifact lacks the expected Cargo VCS/raw-commit provenance")
    generated_at = data.get("generatedAtUtc")
    if not isinstance(generated_at, str):
        raise ValueError("supplemental artifact has no generatedAtUtc value")
    try:
        datetime.fromisoformat(generated_at.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError("supplemental generatedAtUtc is not an ISO-8601 timestamp") from error
    entries = data.get("entries")
    if not isinstance(entries, list):
        raise ValueError("supplemental entries must be an array")

    for entry in entries:
        if not isinstance(entry, dict):
            raise ValueError("supplemental entry must be an object")
        repository = entry.get("repository")
        commit = entry.get("commit")
        if not isinstance(repository, str) or not repository.startswith("https://github.com/"):
            raise ValueError("supplemental repository must be an HTTPS GitHub URL")
        repo_parts = urlsplit(repository.rstrip("/"))
        repo_path = repo_parts.path.strip("/").split("/")
        if (repo_parts.netloc != "github.com" or repo_parts.query or repo_parts.fragment or
                len(repo_path) != 2 or
                any(not part for part in repo_path)):
            raise ValueError(f"invalid supplemental GitHub repository: {repository}")
        if not isinstance(commit, str) or not COMMIT_RE.fullmatch(commit):
            raise ValueError(f"invalid supplemental repository commit: {commit!r}")
        packages = entry.get("packages")
        files = entry.get("files")
        if not isinstance(packages, list) or not packages or not isinstance(files, list):
            raise ValueError("supplemental entries require packages and files arrays")
        for package in packages:
            if not isinstance(package, dict) or not all(
                    isinstance(package.get(key), str) and package[key]
                    for key in ("name", "version", "license")):
                raise ValueError("supplemental package requires exact name, version, and license")
            crate_path = package.get("cratePath")
            if crate_path is not None and not isinstance(crate_path, str):
                raise ValueError("supplemental cratePath must be a string or null")
            if crate_path is not None and crate_path and (
                    crate_path.startswith("/") or "\\" in crate_path or
                    any(part in ("", ".", "..") for part in crate_path.split("/"))):
                raise ValueError(f"unsafe supplemental cratePath: {crate_path!r}")
        for file in files:
            if not isinstance(file, dict):
                raise ValueError("supplemental file entry must be an object")
            for key in ("kind", "role", "path", "sourceUrl", "sha256", "text"):
                if not isinstance(file.get(key), str):
                    raise ValueError(f"supplemental file is missing string field {key}")
            file_path = file["path"]
            path_parts = file_path.split("/")
            if (not file_path or file_path.startswith("/") or "\\" in file_path or
                    any(part in ("", ".", "..") for part in path_parts)):
                raise ValueError(f"unsafe supplemental file path: {file_path!r}")
            if not SHA256_RE.fullmatch(file["sha256"]):
                raise ValueError(f"invalid supplemental SHA-256 for {file_path}")
            raw = file["text"].encode("utf-8")
            if b"\0" in raw or len(raw) > MAX_TEXT:
                raise ValueError(f"invalid supplemental text size/content for {file_path}")
            if not isinstance(file.get("bytes"), int) or isinstance(file.get("bytes"), bool):
                raise ValueError(f"invalid supplemental byte count for {file_path}")
            if len(raw) != file["bytes"]:
                raise ValueError(f"supplemental byte count mismatch for {file_path}")
            digest = hashlib.sha256(raw).hexdigest()
            if digest != file["sha256"]:
                raise ValueError(f"supplemental SHA-256 mismatch for {file_path}")
            expected_url = (
                f"https://raw.githubusercontent.com/{repo_path[0]}/{repo_path[1]}/"
                f"{commit}/{file_path}"
            )
            if file["sourceUrl"] != expected_url:
                raise ValueError(f"supplemental source URL does not match repo/commit/path: {file_path}")

    blockers = data.get("blockers", [])
    observations = data.get("observations", [])
    if (not isinstance(blockers, list) or not all(isinstance(v, str) for v in blockers) or
            not isinstance(observations, list) or not all(isinstance(v, str) for v in observations)):
        raise ValueError("supplemental blockers and observations must be arrays of strings")
    return data


def apply_supplemental(records: list[dict], package_sources: list[dict | None],
                       data: dict) -> tuple[list[str], int]:
    """Attach texts only to package/version/repository/commit exact matches."""
    resolved_text_warnings: set[str] = set()
    matched_file_count = 0
    for entry in data["entries"]:
        repository = entry["repository"].rstrip("/")
        commit = entry["commit"]
        for package in entry["packages"]:
            for index, record in enumerate(records):
                source = package_sources[index]
                if (source is None or record["name"] != package["name"] or
                        record["version"] != package["version"] or
                        record["license"] != package["license"] or
                        not record["repository"] or
                        record["repository"].rstrip("/") != repository or
                        source.get("commit") != commit):
                    continue
                if not entry["files"]:
                    continue
                supplemental_source = {
                    "repository": repository,
                    "commit": commit,
                    "package": {"name": record["name"], "version": record["version"]},
                    "files": [
                        {key: file[key] for key in
                         ("kind", "role", "path", "sourceUrl", "sha256", "bytes", "text")}
                        for file in entry["files"]
                    ],
                }
                record.setdefault("supplementalSources", []).append(supplemental_source)
                matched_file_count += len(entry["files"])
                if not record["licenseFiles"]:
                    resolved_text_warnings.add(
                        f"No readable license/notice text for {record['name']} {record['version']}"
                    )
    return sorted(resolved_text_warnings), matched_file_count


def collect() -> tuple[list[dict], list[str], list[dict | None]]:
    command = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline",
               "--filter-platform", PLATFORM, "--manifest-path", str(MANIFEST)]
    result = subprocess.run(command, cwd=MANIFEST.parent, check=True, capture_output=True, text=True, timeout=90)
    metadata = json.loads(result.stdout)
    packages = {package["id"]: package for package in metadata["packages"]}
    resolve = metadata.get("resolve") or {}
    nodes = {node["id"]: node for node in resolve.get("nodes", [])}
    root_ids = set(metadata.get("workspace_members", []))
    active: set[str] = set()
    pending = list(root_ids)
    while pending:
        package_id = pending.pop()
        if package_id in active:
            continue
        active.add(package_id)
        node = nodes.get(package_id, {})
        for dep in node.get("deps", []):
            kinds = dep.get("dep_kinds", [])
            if any(kind.get("kind") in (None, "build") for kind in kinds):
                pending.append(dep["pkg"])

    records: list[dict] = []
    warnings: list[str] = []
    package_sources: list[dict | None] = []
    repo = ROOT.resolve()
    for package_id in sorted(active):
        package = packages.get(package_id)
        if not package:
            warnings.append(f"Resolved package metadata missing: {package_id}")
            continue
        crate_root = Path(package["manifest_path"]).resolve().parent
        # Wavelane's own crate needs no notice; vendored (patched) third-party
        # crates under vendor/ do.
        if crate_root.is_relative_to(repo) and not crate_root.is_relative_to(repo / "vendor"):
            continue
        files: dict[str, str] = {}
        problems: dict[str, str] = {}
        candidates: set[Path] = set()
        for pattern in LICENSE_NAMES:
            candidates.update(crate_root.glob(pattern))
        declared = package.get("license_file")
        if declared:
            candidate = Path(declared)
            if not candidate.is_absolute():
                candidate = crate_root / candidate
            candidates.add(candidate)
        for candidate in sorted(candidates, key=lambda item: str(item)):
            relative = str(candidate.relative_to(crate_root)) if candidate.is_relative_to(crate_root) else str(candidate)
            text, error = safe_text(candidate, crate_root)
            if error:
                problems[relative] = error
            else:
                files[relative] = text or ""
        record = {
            "name": package["name"], "version": package["version"],
            "license": package.get("license"), "repository": package.get("repository"),
            "licenseFiles": files, "fileProblems": problems,
        }
        if not files:
            warnings.append(f"No readable license/notice text for {package['name']} {package['version']}")
        if not package.get("license") and not declared:
            warnings.append(f"No declared license expression/file for {package['name']} {package['version']}")
        records.append(record)
        package_sources.append({"commit": read_vcs_commit(crate_root)})

    return records, warnings, package_sources


def write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink():
        raise ValueError(f"refusing symlink output: {path}")
    path.write_bytes(data)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--notices", type=Path, default=ROOT / "local" / "THIRD_PARTY_NOTICES.txt")
    parser.add_argument("--manifest", type=Path, default=ROOT / "local" / "third-party-notices.json")
    parser.add_argument(
        "--supplemental", type=Path,
        help="optional verified, offline supplemental-notices JSON artifact",
    )
    args = parser.parse_args()
    def output_path(value: Path) -> Path:
        path = Path(os.path.abspath(value if value.is_absolute() else ROOT / value))
        if os.path.lexists(path) and path.is_symlink():
            parser.error(f"refusing symlink output: {path}")
        if path.parent.resolve() != path.parent:
            parser.error(f"output parent must be canonical: {path.parent}")
        return path

    notices_path = output_path(args.notices)
    manifest_path = output_path(args.manifest)
    supplemental_path = None
    if args.supplemental is not None:
        supplemental_path = Path(os.path.abspath(
            args.supplemental if args.supplemental.is_absolute() else ROOT / args.supplemental
        ))
        if os.path.lexists(supplemental_path) and supplemental_path.is_symlink():
            parser.error(f"refusing symlink supplemental input: {supplemental_path}")
        try:
            supplemental_path.resolve(strict=True)
        except OSError as error:
            parser.error(f"cannot read supplemental artifact: {error}")
    if notices_path == manifest_path:
        parser.error("notice text and JSON manifest must use different paths")
    if supplemental_path in (notices_path, manifest_path):
        parser.error("supplemental input must differ from both output paths")
    private_local = (ROOT / "local").resolve()
    for path in (notices_path, manifest_path):
        try:
            path.relative_to(ROOT.resolve())
        except ValueError:
            continue
        try:
            path.relative_to(private_local)
        except ValueError:
            parser.error("repository-local outputs are limited to the ignored local/ directory")
    supplemental = None
    if supplemental_path is not None:
        try:
            supplemental = load_supplemental(supplemental_path)
        except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
            parser.error(f"invalid supplemental artifact: {error}")
    records, warnings, package_sources = collect()
    if supplemental is not None:
        resolved_warnings, supplemental_file_count = apply_supplemental(
            records, package_sources, supplemental
        )
        warnings = [warning for warning in warnings if warning not in resolved_warnings]
        warnings.extend(
            f"Supplemental unresolved blocker: {blocker}"
            for blocker in supplemental["blockers"]
        )
    else:
        supplemental_file_count = 0
    lines = ["Wavelane third-party license and notice texts", "", "Generated from locked offline Cargo metadata for x86_64-unknown-linux-gnu, default features.", ""]
    for record in records:
        lines.extend(["=" * 78, f"{record['name']} {record['version'] or ''}".strip(),
                      f"License expression: {record['license'] or 'not declared'}",
                      f"Repository: {record['repository'] or 'not declared'}", ""])
        if record["licenseFiles"]:
            for filename, text in record["licenseFiles"].items():
                lines.extend([f"--- {filename} ---", text.rstrip(), ""])
        elif not record.get("supplementalSources"):
            lines.append("No license text was found; see the JSON manifest warnings.\n")
        for source in record.get("supplementalSources", []):
            package = source["package"]
            lines.extend([
                "Supplemental verified source:",
                f"Package: {package['name']} {package['version']}",
                f"Repository: {source['repository']}",
                f"Repository commit: {source['commit']}",
                "",
            ])
            for file in source["files"]:
                lines.extend([
                    f"--- supplemental {file['path']} ({file['kind']}; {file['role']}) ---",
                    f"Source: {file['sourceUrl']}",
                    f"SHA-256: {file['sha256']} ({file['bytes']} bytes)",
                    file["text"].rstrip(),
                    "",
                ])
    if warnings:
        lines.extend(["=" * 78, "Unresolved review warnings", ""])
        lines.extend(f"- {warning}" for warning in warnings)
        lines.append("")
    notice_bytes = ("\n".join(lines) + "\n").encode()
    manifest = {
        "generatedAtUtc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "platform": PLATFORM, "features": [], "packages": records, "warnings": warnings,
        "noticeTextSha256": hashlib.sha256(notice_bytes).hexdigest(),
    }
    if supplemental is not None:
        manifest["supplementalArtifact"] = {
            "generatedAtUtc": supplemental["generatedAtUtc"],
            "provenance": supplemental["provenance"],
            "matchedVerifiedFileCount": supplemental_file_count,
            "blockers": supplemental["blockers"],
            "observations": supplemental["observations"],
        }
    write(notices_path, notice_bytes)
    write(manifest_path, (json.dumps(manifest, indent=2, ensure_ascii=False) + "\n").encode())
    print(f"Wrote {notices_path} and {manifest_path}")
    if warnings:
        print(f"Review {len(warnings)} unresolved license-text warning(s); this is not legal clearance.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
