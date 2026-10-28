"""Offline package dependency provenance and verbatim upstream license notices."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib
import zipfile


NOTICE_NAME = re.compile(
    r"^(?:licen[cs]es?|copying|notices?|copyright|unlicense)(?:[._-].*)?$", re.I
)
EXTRA_NOTICE_NAME = re.compile(
    r"(?:^|[-_.])(?:licen[cs]e|notice|copyright)(?:[-_.]|$)|^(?:OFL|UFL)\.txt$", re.I
)
NOTICE_DIRS = {"licenses", "licences", "notices"}
SKIP_DIRS = {".git", "node_modules", "target", "__pycache__"}


def filesystem_path(path: Path) -> Path:
    """Use Windows extended paths so deep source checkouts retain native access."""
    absolute = os.path.abspath(path)
    if os.name != "nt" or absolute.startswith("\\\\?\\"):
        return Path(absolute)
    if absolute.startswith("\\\\"):
        return Path("\\\\?\\UNC\\" + absolute[2:])
    return Path("\\\\?\\" + absolute)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def file_record(path: Path, output: Path) -> dict:
    return {
        "path": path.relative_to(output).as_posix(),
        "bytes": path.stat().st_size,
        "sha256": digest(path),
    }


def notice_files(
    source: Path, declared: str | None = None
) -> tuple[list[Path], list[str]]:
    """Keep nested vendored notices; never follow links out of a source package."""
    source = source.resolve(strict=True)
    selected = set()
    issues = []
    for directory, dirs, files in os.walk(source, followlinks=False):
        dirs[:] = sorted(
            d
            for d in dirs
            if d not in SKIP_DIRS
            and not (Path(directory) / d).is_symlink()
            and not (Path(directory) / d).is_junction()
        )
        for name in sorted(files):
            path = Path(directory) / name
            relative = path.relative_to(source)
            if (
                NOTICE_NAME.match(name)
                or EXTRA_NOTICE_NAME.search(name)
                or any(part.lower() in NOTICE_DIRS for part in relative.parts[:-1])
                or ("fonts" in relative.parts[:-1] and path.suffix.lower() == ".txt")
            ):
                resolved = path.resolve(strict=True)
                if path.is_symlink() or not resolved.is_relative_to(source):
                    issues.append(
                        "Notice is a symbolic link or resolves outside the package and was not copied: "
                        + relative.as_posix()
                    )
                else:
                    selected.add(resolved)
    if declared:
        path = source / declared
        resolved = path.resolve()
        if not resolved.is_relative_to(source) or path.is_symlink():
            issues.append(
                "Declared license file is outside the package or is a symbolic link."
            )
        elif not path.is_file():
            issues.append(
                "Declared license file is absent from the installed package: "
                + declared
            )
        else:
            selected.add(resolved)
    return sorted(selected), issues


def copy_notices(
    source: Path, output: Path, key: str, declared: str | None = None
) -> tuple[list[dict], list[str]]:
    selected, issues = notice_files(source, declared)
    records = []
    source = source.resolve(strict=True)
    for path in selected:
        resolved = path.resolve(strict=True)
        if path.is_symlink() or not resolved.is_relative_to(source):
            raise RuntimeError(
                "A selected notice no longer resolves inside its source package."
            )
        relative = resolved.relative_to(source)
        # Flatten by the complete content digest: nested upstream paths exceeded Windows
        # MAX_PATH in a real package build. sourcePath below retains their provenance.
        content = resolved.read_bytes()
        content_hash = hashlib.sha256(content).hexdigest()
        destination = output / "third-party" / "texts" / (content_hash + ".txt")
        if destination.is_symlink() or not destination.resolve().is_relative_to(
            output.resolve()
        ):
            raise RuntimeError(
                "A notice destination resolves outside the package output."
            )
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(content)
        records.append(
            {"sourcePath": relative.as_posix(), **file_record(destination, output)}
        )
    if not records:
        issues.append(
            "No matching upstream license or notice text was present in this installed package."
        )
    return records, issues


def runtime_closure(metadata: dict, root_name: str) -> set[str]:
    """Cargo normal edges describe runtime candidates, not a binary linker audit."""
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    roots = [
        p["id"]
        for p in metadata["packages"]
        if p["name"] == root_name and p["id"] in metadata["workspace_members"]
    ]
    if len(roots) != 1:
        raise RuntimeError("Expected one workspace package named " + root_name)
    pending = roots[:]
    result = set()
    while pending:
        identity = pending.pop()
        if identity in result:
            continue
        result.add(identity)
        for dependency in nodes[identity]["deps"]:
            target = packages[dependency["pkg"]]
            if any("proc-macro" in item["kind"] for item in target["targets"]):
                continue
            if any(kind["kind"] is None for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    return result


def cargo_metadata(cargo: str, root: Path, target: str, private: bool) -> dict:
    arguments = [
        cargo,
        "metadata",
        "--offline",
        "--locked",
        "--format-version",
        "1",
        "--filter-platform",
        target,
    ]
    if private:
        raise ValueError("This source distribution does not include private helpers.")
    result = subprocess.run(
        arguments,
        cwd=root,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
        shell=False,
    )
    return json.loads(result.stdout)


def package_key(ecosystem: str, name: str, version: str, identity: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9._-]", "_", name + "-" + version)
    return (
        ecosystem
        + "-"
        + safe
        + "-"
        + hashlib.sha256(identity.encode()).hexdigest()[:12]
    )


def copy_workspace_license(
    package: dict, root: Path, output: Path
) -> tuple[list[dict], list[str], str | None]:
    """Copy the declared first-party license from within the workspace."""
    declared = package.get("license_file")
    if not declared:
        return [], [], None
    source = Path(package["manifest_path"]).parent / declared
    resolved = source.resolve(strict=True)
    root = root.resolve(strict=True)
    if source.is_symlink() or not resolved.is_relative_to(root):
        raise RuntimeError(
            "The workspace license is outside the workspace or is a symbolic link."
        )
    relative = resolved.relative_to(root).as_posix()
    content = resolved.read_bytes()
    checksum = hashlib.sha256(content).hexdigest()
    destination = output / "third-party/texts" / (checksum + ".txt")
    if destination.is_symlink() or not destination.resolve().is_relative_to(
        output.resolve()
    ):
        raise RuntimeError("The workspace notice destination is outside the package.")
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(content)
    return [{"sourcePath": relative, **file_record(destination, output)}], [], relative


def cargo_records(
    metadata: list[dict],
    artifacts: dict[str, list[dict]],
    output: Path,
    lock_checksums: dict,
) -> list[dict]:
    packages = {p["id"]: p for value in metadata for p in value["packages"]}
    observed = {
        role: {item["package_id"] for item in items}
        for role, items in artifacts.items()
    }
    host_runtime = runtime_closure(metadata[0], "herbert")
    guest_runtime = runtime_closure(metadata[1], "herbert-plugins")
    records = []
    for identity in sorted(set().union(*observed.values())):
        if identity not in packages:
            raise RuntimeError(
                "A compiled package was missing from offline Cargo metadata: "
                + identity
            )
        package = packages[identity]
        source = Path(package["manifest_path"]).parent.resolve(strict=True)
        roles = []
        if identity in observed["nativeHost"] and identity in host_runtime:
            roles.append("nativeHostDependency")
        if identity in observed["wasmPlugins"] and identity in guest_runtime:
            roles.append("wasmPluginDependency")
        if not roles:
            roles.append("buildToolingOnly")
        origin = package.get("source") or "workspace"
        public_identity = origin + "#" + package["name"] + "@" + package["version"]
        key = package_key("cargo", package["name"], package["version"], public_identity)
        declared_file = package.get("license_file")
        if origin == "workspace":
            texts, issues, declared_file = copy_workspace_license(
                package, Path(metadata[0]["workspace_root"]), output
            )
        else:
            texts, issues = copy_notices(source, output, key, declared_file)
        checksum = lock_checksums.get((origin, package["name"], package["version"]))
        records.append(
            {
                "ecosystem": "cargo",
                "name": package["name"],
                "version": package["version"],
                "source": origin,
                "repository": package.get("repository"),
                "declaredLicense": package.get("license"),
                "declaredLicenseFile": declared_file,
                "roles": roles,
                "observedBuildStages": sorted(
                    stage for stage, ids in observed.items() if identity in ids
                ),
                "cargoLockChecksum": checksum,
                "manifestSha256": digest(source / "Cargo.toml"),
                "texts": texts,
                "issues": issues,
            }
        )
    return records


def npm_records(root: Path, assembly_source: Path | None, output: Path) -> list[dict]:
    reference = root / "examples/system-info/assemblyscript/package-lock.json"
    lock_path = assembly_source / "package-lock.json"
    if digest(reference) != digest(lock_path):
        raise RuntimeError(
            "The AssemblyScript source lock differs from the pinned project lock."
        )
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    records = []
    for location, info in sorted(lock["packages"].items()):
        if not location:
            continue
        source = (assembly_source / location).resolve(strict=True)
        if not source.is_relative_to(assembly_source.resolve()):
            raise RuntimeError(
                "An installed npm dependency points outside its source directory."
            )
        manifest = source / "package.json"
        package = json.loads(manifest.read_text(encoding="utf-8"))
        expected_name = location.rsplit("node_modules/", 1)[-1]
        if (
            package["name"] != info.get("name", expected_name)
            or package["version"] != info["version"]
        ):
            raise RuntimeError(
                "Installed npm dependency version differs from its lock: " + location
            )
        name = package["name"]
        # The shipped example imports json-as; its installed runtime imports xjb-as.
        # AssemblyScript supplies both the compiler and generated module's standard runtime.
        roles = (
            ["assemblyScriptRuntimeInput"]
            if name in {"assemblyscript", "json-as", "xjb-as"}
            else ["buildOrFormattingToolingOnly"]
        )
        if name in {"assemblyscript", "json-as"}:
            roles.append("assemblyScriptCompilerOrTransform")
        key = package_key(
            "npm", name, package["version"], info.get("resolved", location)
        )
        texts, issues = copy_notices(source, output, key)
        records.append(
            {
                "ecosystem": "npm",
                "name": name,
                "version": package["version"],
                "source": info.get("resolved"),
                "repository": package.get("repository"),
                "declaredLicense": package.get("license", info.get("license")),
                "roles": roles,
                "lockDevDependency": info.get("dev", False),
                "npmIntegrity": info.get("integrity"),
                "manifestSha256": digest(manifest),
                "texts": texts,
                "issues": issues,
            }
        )
    return records


def supplement_records(root: Path, output: Path, records: list[dict]) -> None:
    """Use reviewed, exact-version upstream texts offline; never guess by SPDX."""
    directory = filesystem_path(root / "assets/third-party-supplemental")
    manifest = directory / "manifest.json"
    if not manifest.is_file():
        return
    supplements = json.loads(manifest.read_text(encoding="utf-8"))
    for record in records:
        if record["source"] == "workspace":
            record["noticeDisposition"] = (
                "First-party workspace package. Its declared license applies separately from third-party licenses."
            )
            continue
        if record["texts"]:
            continue
        match = next(
            (
                p
                for p in supplements["packages"]
                if p["ecosystem"] == record["ecosystem"]
                and p["name"] == record["name"]
                and p["version"] == record["version"]
            ),
            None,
        )
        if match is None:
            continue
        for field in ["cargoLockChecksum", "manifestSha256"]:
            if match[field] != record[field]:
                raise RuntimeError(
                    "Supplemental license package identity changed: " + record["name"]
                )
        for text in match["texts"]:
            source = (directory / text["path"]).resolve(strict=True)
            if not source.is_relative_to(directory.resolve()):
                raise RuntimeError(
                    "Supplemental notice is outside its source directory."
                )
            data = source.read_bytes()
            if (
                len(data) != text["bytes"]
                or hashlib.sha256(data).hexdigest() != text["sha256"]
            ):
                raise RuntimeError(
                    "Supplemental notice integrity failed: " + record["name"]
                )
            target = output / "third-party/texts" / (text["sha256"] + ".txt")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            record["texts"].append(
                {
                    "sourcePath": text["sourcePath"],
                    **file_record(target, output),
                    "provenance": {
                        k: v
                        for k, v in text.items()
                        if k not in {"path", "sourcePath", "bytes", "sha256"}
                    },
                }
            )
        record["observedSourceIssues"] = record["issues"]
        record["issues"] = []
        record["noticeDisposition"] = match["resolution"]
        record["supplementalCommit"] = match["commit"]


def write_uncompressed_inventory(
    root: Path,
    output: Path,
    cargo: str,
    native_target: str,
    assembly_source: Path | None,
    private: bool,
    artifacts: dict[str, list[dict]],
) -> list[dict]:
    cargo_version = subprocess.run(
        [cargo, "-vV"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
        shell=False,
    ).stdout
    build_host = next(
        line.removeprefix("host: ").strip()
        for line in cargo_version.splitlines()
        if line.startswith("host: ")
    )
    metadata = [
        cargo_metadata(cargo, root, native_target, private),
        cargo_metadata(cargo, root, "wasm32-unknown-unknown", private),
    ]
    if build_host != native_target:
        metadata.append(cargo_metadata(cargo, root, build_host, private))
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    checksums = {
        (item.get("source", "workspace"), item["name"], item["version"]): item.get(
            "checksum"
        )
        for item in lock["package"]
    }
    records = cargo_records(metadata, artifacts, output, checksums)
    if assembly_source is not None:
        records += npm_records(root, assembly_source, output)
    supplement_records(root, output, records)
    workspace_package = next(
        p
        for p in metadata[0]["packages"]
        if p["name"] == "herbert" and p["id"] in metadata[0]["workspace_members"]
    )
    workspace_license = workspace_package.get("license")
    workspace_license_file = None
    if workspace_package.get("license_file"):
        declared = (
            Path(workspace_package["manifest_path"]).parent
            / workspace_package["license_file"]
        )
        workspace_license_file = (
            declared.resolve(strict=True).relative_to(root.resolve()).as_posix()
        )
    inventory = {
        "schemaVersion": 1,
        "nativeTarget": native_target,
        "privateHelper": private,
        "productLicenseDeclaration": workspace_license,
        "productLicenseFile": workspace_license_file,
        "scope": "Packages observed during this package build. Normal dependency graph roles are not an exact linker or code-retention audit. Test-only dependencies not used by the package build are omitted. Rust/system toolchain components and dynamically supplied operating-system libraries are outside this package-source inventory.",
        "inputs": [
            {"path": "Cargo.lock", "sha256": digest(root / "Cargo.lock")},
            *(
                [
                    {
                        "path": "examples/system-info/assemblyscript/package-lock.json",
                        "sha256": digest(assembly_source / "package-lock.json"),
                    }
                ]
                if assembly_source is not None
                else []
            ),
        ],
        "cargoVersion": cargo_version.strip(),
        "packages": records,
    }
    directory = output / "third-party"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "dependencies.json").write_text(
        json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    lines = [
        "# Dependency and upstream notice inventory",
        "",
        (
            "Herbert uses the license in " + workspace_license_file + "."
            if workspace_license_file
            else "Herbert declares "
            + str(workspace_license or "no license expression")
            + "."
        )
        + " This inventory does not change the product license.",
        "",
        inventory["scope"],
        "",
        "License expressions are copied from upstream metadata. Text files are copied verbatim from installed pinned sources. Missing text is recorded; no license grant or compliance conclusion is inferred. JSON records include exact source, lock/checksum evidence and hashes for every copied file.",
        "",
        "| Package | Roles | Declared license | Text files |",
        "| --- | --- | --- | --- |",
    ]
    for item in records:
        cells = [
            item["ecosystem"] + ":" + item["name"] + " " + item["version"],
            ", ".join(item["roles"]),
            str(item["declaredLicense"] or "not declared"),
            str(len(item["texts"])),
        ]
        lines.append(
            "| "
            + " | ".join(
                value.replace("|", "\\|").replace("\n", " ") for value in cells
            )
            + " |"
        )
    (directory / "README.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return [
        file_record(path, output)
        for path in sorted(directory.rglob("*"))
        if path.is_file()
    ]


def archive_inventory(source: Path, output: Path) -> list[dict]:
    """Compress a complete inventory without changing any upstream text bytes.

    Shared identical texts have one readable archive member; each package retains
    its own sourcePath and hash in dependencies.json. ZIP members avoid the real
    Windows MAX_PATH failure that required flattened staging filenames.
    """
    inventory = json.loads(
        (source / "third-party/dependencies.json").read_text(encoding="utf-8")
    )
    members = {}
    by_digest = {}
    readable_notices = {}
    for package in inventory["packages"]:
        key = package_key(
            package["ecosystem"],
            package["name"],
            package["version"],
            str(package["source"]),
        )
        for notice in package["texts"]:
            path = (source / notice["path"]).resolve(strict=True)
            if not path.is_relative_to(source.resolve()):
                raise RuntimeError("Notice inventory points outside its package.")
            content = path.read_bytes()
            checksum = hashlib.sha256(content).hexdigest()
            if checksum != notice["sha256"] or len(content) != notice["bytes"]:
                raise RuntimeError("Notice bytes differ from the original inventory.")
            upstream = Path(notice["sourcePath"])
            if (
                upstream.is_absolute()
                or ".." in upstream.parts
                or "\\" in notice["sourcePath"]
            ):
                raise RuntimeError(
                    "Notice source path is not a portable relative path."
                )
            member = by_digest.setdefault(
                checksum, "licenses/" + key + "/" + upstream.as_posix()
            )
            if member in members and members[member] != content:
                raise RuntimeError("Conflicting notice archive member.")
            members[member] = content
            notice["path"] = member
            # Apache section 4(d) calls for readable NOTICE attribution; bundled
            # OFL/UFL fonts require easily viewed license/copyright information.
            # Retain these verbatim in a directly readable companion as well.
            if (
                re.match(r"^notices?(?:[._-].*)?$", upstream.name, re.I)
                or "fonts" in upstream.parts
            ):
                readable_notices.setdefault(checksum, (member, content))
    inventory["schemaVersion"] = 2
    inventory["textLocation"] = (
        "Paths in texts refer to members of THIRD-PARTY-LICENSES.zip. Identical texts share a member; sourcePath remains package-relative."
    )
    members["dependencies.json"] = (
        json.dumps(inventory, indent=2, ensure_ascii=False) + "\n"
    ).encode("utf-8")
    members["README.md"] = (source / "third-party/README.md").read_bytes()
    directory = output / "third-party"
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / "THIRD-PARTY-LICENSES.zip"
    # Standard DEFLATE is readable by Windows Explorer. The library default
    # compression level avoids introducing an unrelated tuning policy.
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as zipped:
        for name, content in sorted(members.items()):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            zipped.writestr(info, content)
    with zipfile.ZipFile(archive) as zipped:
        if set(zipped.namelist()) != set(members):
            raise RuntimeError("Notice archive inventory mismatch.")
        for name, content in members.items():
            if zipped.read(name) != content:
                raise RuntimeError("Notice archive byte verification failed: " + name)
    (directory / "README.md").write_text(
        "# Third-party licenses and notices\n\n"
        "Open [THIRD-PARTY-LICENSES.zip](THIRD-PARTY-LICENSES.zip) with Windows Explorer or any standard ZIP reader. "
        "It contains the complete upstream license and notice texts under `licenses/`, "
        "the package list in `README.md`, and provenance plus SHA-256 hashes in `dependencies.json`.\n\n"
        "`THIRD-PARTY-NOTICES.txt` also keeps upstream NOTICE files and font license/copyright texts directly readable.\n\n"
        "Texts are preserved byte for byte. Identical texts share one readable member; "
        "the inventory retains every package's original source path. Keep this archive with Herbert. "
        "Existing missing-text findings remain recorded in the inventory; compression does not resolve them.\n",
        encoding="utf-8",
    )
    readable = directory / "THIRD-PARTY-NOTICES.txt"
    with readable.open("xb") as stream:
        stream.write(
            b"Third-party attribution and font notices. Full license inventory: THIRD-PARTY-LICENSES.zip\n"
        )
        for name, content in readable_notices.values():
            stream.write(("\n===== " + name + " =====\n").encode("utf-8"))
            stream.write(content)
            stream.write(b"\n")
    return [
        file_record(path, output)
        for path in [directory / "README.md", readable, archive]
    ]


def write_inventory(
    root, output, cargo, native_target, assembly_source, private, artifacts
):
    with tempfile.TemporaryDirectory(prefix="herbert-notices-") as temporary:
        staged = Path(temporary)
        write_uncompressed_inventory(
            root, staged, cargo, native_target, assembly_source, private, artifacts
        )
        return archive_inventory(staged, output)
