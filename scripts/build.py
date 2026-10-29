"""Build a Herbert folder with the approved rewritten plugins.

By default, replace the unchanged development candidate outside the source tree.
Use --output for a new retained release/evidence directory. A package build must
not replace a user's portable plugin state. Private helper source is not included.
"""

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from package_licenses import write_inventory
from build_workspace import compiler_cache, run


ROOT = Path(__file__).resolve().parents[1]


# Collectors included in the standard package.
RELEASE_CATEGORIES = (
    "ud-codes",
    "ud-schema",
    "extended-properties",
    "baqs-and-ubaqs",
    "bpm-directives",
    "functions",
    "classic-customizations",
    "classic-personalizations",
    "classic-dashboards",
    "kinetic-applications-and-layers",
    "kinetic-personalizations",
    "searches-and-navigation",
    "menus-and-launch-definitions",
    "report-definitions",
    "report-styles-and-layouts",
    "report-behavior",
    "configurators",
    "business-rules-and-mappings",
    "custom-resources",
    "programs-and-extensions",
    "solutions-and-packages",
    "automation-and-integrations",
    "access-and-assignments",
    "configuration-references",
)

# User-facing documents only. Engineering and category notes stay in the repository.
HOST_DOCUMENTS = (
    ("docs/RELEASE-AGENTS.md", "AGENTS.md"),
    ("docs/release/mcp.claude-desktop.example.json", "mcp.claude-desktop.example.json"),
    ("docs/app/technical-guide.json", "docs/app/technical-guide.json"),
    ("docs/app/user-guide.json", "docs/app/user-guide.json"),
    ("docs/RUN-DIAGNOSTICS.md", "RUN-DIAGNOSTICS.md"),
    ("skills/herbert/SKILL.md", "skills/herbert/SKILL.md"),
    ("skills/codex/herbert/SKILL.md", "skills/codex/herbert/SKILL.md"),
    ("README.md", "README.md"),
    ("CHANGELOG.md", "CHANGELOG.md"),
    ("docs/Herbert-Guide.md", "docs/Herbert-Guide.md"),
    ("docs/MCP-GUIDE.md", "docs/MCP-GUIDE.md"),
)
HOST_ASSETS = (
    ("assets/branding/herbert-vacuum.png", "assets/branding/herbert-vacuum.png"),
    ("assets/branding/creator-avatar.png", "assets/branding/creator-avatar.png"),
    ("assets/branding/herbert.ico", "assets/branding/herbert.ico"),
)


def standard_plugins(source_directory: Path) -> list[tuple[str, str]]:
    """Select rewritten collectors and fail if an approved source is missing."""
    selected = [(category, category) for category in RELEASE_CATEGORIES]
    for binary, _ in selected:
        if not (source_directory / (binary + ".rs")).is_file():
            raise RuntimeError("An approved plugin source is missing: " + binary)
    return selected


def standard_plugin_features(category: str) -> list[str]:
    """Compiled collection support for BAQs and UBAQs, BPM directives and Functions is part of the standard package.

    Every plugin also gets its own no-op `plugin-<category>` feature. Each build compiles the shared library for one
    category (HERBERT_PLUGIN_CATEGORY); a distinct feature set gives each category its own fingerprinted library in
    the shared cache, so an unchanged plugin is not rebuilt because another plugin was built in between."""
    compiled = {"baqs-and-ubaqs": "ubaq-compiled", "bpm-directives": "ordinary-bpm-compiled", "functions": "function-compiled"}.get(category)
    return ["--features", ",".join([f"plugin-{category}", *([compiled] if compiled else [])])]


def standard_host_documents(root: Path) -> list[tuple[Path, Path]]:
    """Select the host documents that every package must contain."""
    selected = [(root / source, Path(destination)) for source, destination in HOST_DOCUMENTS]
    missing = [source.relative_to(root).as_posix() for source, _ in selected if not source.is_file()]
    if missing:
        raise RuntimeError("A required host document is missing: " + ", ".join(missing))
    return selected


def cargo_home(environment: dict) -> Path:
    value = environment.get("CARGO_HOME")
    return Path(value).resolve() if value else (Path.home() / ".cargo").resolve()


def compiler_environment() -> dict:
    """Keep caller flags and replace local source paths in compiled diagnostics."""
    environment = os.environ.copy()
    environment.setdefault("CARGO_TARGET_DIR", str(compiler_cache()))
    encoded = environment.get("CARGO_ENCODED_RUSTFLAGS")
    flags = (
        (encoded.split("\x1f") if encoded else [])
        if encoded is not None
        else environment.get("RUSTFLAGS", "").split()
    )
    prefixes = [
        (
            cargo_home(environment),
            "/cargo",
        ),
        (ROOT, "/herbert"),
    ]
    if environment.get("CARGO_TARGET_DIR"):
        prefixes.append((Path(environment["CARGO_TARGET_DIR"]).resolve(), "/build"))
    for source, replacement in prefixes:
        for spelling in sorted({str(source), source.as_posix()}):
            flags.append(f"--remap-path-prefix={spelling}={replacement}")
    environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flags)
    return environment


def native_compiler_environment(target: str) -> dict:
    """Map native MSVC assertion filenames without changing assertion behavior."""
    environment = compiler_environment()
    if target.endswith("-msvc"):
        cargo_directory = cargo_home(environment)
        # The installed MSVC probe requires both flags to map narrow and wide __FILE__ strings.
        # Keep caller flags. The final binary check verifies that mapping actually took effect.
        flags = subprocess.list2cmdline(
            [
                "/experimental:deterministic",
                f"/pathmap:{cargo_directory}=/cargo",
            ]
        )
        # cc tracks CFLAGS, but does not track _CL_. This also invalidates native
        # objects built before path mapping was enabled. Cargo-home changes are
        # separately tracked by the Rust remap flags above.
        environment["CFLAGS"] = (
            environment.get("CFLAGS", "") + " /experimental:deterministic"
        ).strip()
        previous = environment.get("_CL_", "")
        environment["_CL_"] = (previous + " " + flags).strip()
    return environment


def cargo_artifacts(
    cargo: str, args: list[str], environment: dict | None = None
) -> list[dict]:
    result = subprocess.run(
        [cargo, "build", "--locked", "--message-format=json", *args],
        cwd=ROOT,
        text=True,
        capture_output=True,
        shell=False,
        env=environment,
    )
    sys.stderr.write(result.stderr)
    messages = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
    for message in messages:
        if message.get("reason") == "compiler-message":
            sys.stderr.write(message["message"].get("rendered", ""))
    result.check_returncode()
    return [
        message for message in messages if message.get("reason") == "compiler-artifact"
    ]


def copy_file(source: Path, output: Path, relative: Path) -> dict:
    destination = output / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    # Native Linux/macOS launch requires the compiler's executable mode. Copy
    # content and mode; plain copyfile silently produces a non-launchable host.
    shutil.copy(source, destination)
    return file_record(destination, output)


def file_record(file: Path, output: Path) -> dict:
    with file.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {
        "path": file.relative_to(output).as_posix(),
        "bytes": file.stat().st_size,
        "sha256": digest,
    }


def executable_from(artifacts: list[dict], name: str) -> Path:
    for item in artifacts:
        if item["target"]["name"] == name and item.get("executable"):
            return Path(item["executable"])
    raise RuntimeError(f"Cargo did not report the {name} executable.")


def verify_compiler_paths(executable: Path, environment: dict) -> None:
    """Reject source and cache prefixes that remain in the compiled executable."""
    prefixes = [
        ROOT,
        cargo_home(environment),
    ]
    if environment.get("CARGO_TARGET_DIR"):
        prefixes.append(Path(environment["CARGO_TARGET_DIR"]).resolve())
    if environment.get("CARGO_HOME"):
        prefixes.append(Path(environment["CARGO_HOME"]).absolute())
    data = executable.read_bytes()
    for prefix in prefixes:
        for spelling in {str(prefix), prefix.as_posix()}:
            if any(
                spelling.encode(encoding) in data for encoding in ("utf-8", "utf-16le")
            ):
                raise RuntimeError(
                    "The native executable retains a local build path. Check native compiler path-map support."
                )



def managed_companion(source: Path, native_target: str) -> tuple[Path, dict]:
    """Explicit pinned input; no download, PATH discovery or first-use build."""
    rid = {"x86_64-pc-windows-msvc": "win-x64", "x86_64-unknown-linux-gnu": "linux-x64"}.get(native_target)
    if rid is None:
        raise RuntimeError("No supported managed companion for " + native_target)
    directory = source.resolve() / rid
    manifest = json.loads((directory / "companion.json").read_text(encoding="utf-8"))
    executable = "herbert-managed-decompiler.exe" if rid == "win-x64" else "herbert-managed-decompiler"
    if (manifest.get("protocolVersion") != 1 or manifest.get("engineId") != "ilspy-11.1.0.9782-herbert-v1" or manifest.get("targetRid") != rid or manifest.get("executable") != executable or not isinstance(manifest.get("cancelGraceMilliseconds"), int) or manifest["cancelGraceMilliseconds"] <= 0):
        raise RuntimeError("Incompatible companion identity or control contract.")
    path = directory / executable
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("The companion is not an ordinary native executable.")
    data = path.read_bytes()
    if len(data) != manifest.get("bytes") or hashlib.sha256(data).hexdigest() != manifest.get("sha256"):
        raise RuntimeError("The companion differs from its pinned bytes.")
    return directory, manifest


def precompile_guests(executable: Path, output: Path, files: list[dict], cache: Path | None = None) -> dict:
    """Precompile bundled guests with the packaged host's own engine; bind each to its .wasm.

    With a cache folder, a .cwasm is reused when the same engine already precompiled identical .wasm bytes
    (`herbert precompile` is byte-deterministic). The cache keeps only the entries this build used, so it never grows
    beyond one package's guests."""
    with tempfile.TemporaryDirectory() as scratch:
        def host(*arguments: str) -> str:
            return subprocess.run([str(executable), "precompile", *arguments], cwd=scratch, check=True, capture_output=True, text=True, shell=False).stdout.strip()

        engine = host("--engine")
        if not engine:
            raise RuntimeError("The host reported no precompiled-engine fingerprint.")
        folder = cache / hashlib.sha256(engine.encode()).hexdigest() if cache else None
        used, hits, misses = set(), 0, 0
        bindings = {}
        for record in list(files):
            relative = Path(record["path"])
            if len(relative.parts) != 3 or relative.parts[0] != "plugins" or relative.suffix != ".wasm" or relative.stem != relative.parts[1]:
                continue
            cwasm = relative.with_suffix(".cwasm")
            cached = folder / (record["sha256"] + ".cwasm") if folder else None
            if cached and cached.is_file():
                shutil.copyfile(cached, output / cwasm)
                hits += 1
            else:
                host(str(output / relative), str(output / cwasm))
                misses += 1
                if cached:
                    cached.parent.mkdir(parents=True, exist_ok=True)
                    staged = cached.with_suffix(".partial")
                    shutil.copyfile(output / cwasm, staged)
                    os.replace(staged, cached)
            if cached:
                used.add(cached.name)
            files.append(file_record(output / cwasm, output))
            bindings[relative.as_posix()] = {"cwasm": cwasm.as_posix(), "engine": engine}
    if cache:
        # Entries for other engines or for guests no longer built are not reused.
        for stale in [p for p in cache.rglob("*") if p.is_file() and (p.parent != folder or p.name not in used)]:
            stale.unlink()
        print(f"Precompiled guests: {hits} reused, {misses} compiled")
    return bindings


def final_file_records(output: Path, records: list[dict]) -> list[dict]:
    """Selected plugin assets replace their base sibling at the same path."""
    selected = {record["path"]: record for record in records}
    for relative, record in selected.items():
        if file_record(output / relative, output) != record:
            raise RuntimeError(f"Final package member differs from its recorded bytes: {relative}")
    return list(selected.values())

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cargo", default="cargo", help="The Cargo executable. The default uses PATH."
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="New retained package folder; default: managed external development candidate.",
    )
    parser.add_argument("--target", help="An optional native Rust target.")
    parser.add_argument(
        "--debug", action="store_true", help="Build a development package."
    )
    parser.add_argument("--managed-decompilation-companions", type=Path, required=True, help="Required pinned companion root with RID/companion.json and native binary. Standard packages include compiled support for BAQs and UBAQs, BPM directives and Functions. No download or PATH fallback.")
    parser.add_argument("--ordinary-bpm-compiled", action="store_true", default=True, help="Compatibility flag. Standard packages always include compiled BPM support.")
    parser.add_argument("--ubaq-compiled", action="store_true", default=True, help="Compatibility flag. Standard packages always include compiled UBAQ support.")
    args = parser.parse_args()
    args.output = args.output.resolve()
    for name in ("cargo",):
        executable = shutil.which(getattr(args, name))
        if executable is None:
            parser.error(
                f"The {name} executable is unavailable. Install it or supply --{name}."
            )
        setattr(args, name, executable)
    if not (ROOT / "LICENSE.md").is_file():
        parser.error("The first-party LICENSE.md file is missing.")
    if args.output.exists():
        parser.error(
            "Use a new output directory. Existing files and plugin state are preserved."
        )

    native_target = args.target or next(line.removeprefix("host: ") for line in subprocess.check_output([args.cargo, "-vV"], text=True, shell=False).splitlines() if line.startswith("host: "))
    managed = managed_companion(args.managed_decompilation_companions, native_target)
    profile = [] if args.debug else ["--release"]
    target = ["--target", args.target] if args.target else []
    # Cargo reports only artifacts from this build. A glob over a shared cache
    # could accidentally include an earlier private-feature adapter.
    # Each invocation compiles the shared source for exactly one plugin. Building
    # all bins together would unify the library and retain unrelated collectors.
    guests = []
    selections = standard_plugins(ROOT / "crates/herbert-plugins/src/bin")
    for binary, category in selections:
        environment = compiler_environment()
        environment["HERBERT_PLUGIN_CATEGORY"] = category
        guests.extend(
            cargo_artifacts(
                args.cargo,
                [
                    "--package",
                    "herbert-plugins",
                    "--bin",
                    binary,
                    "--release",
                    "--target",
                    "wasm32-unknown-unknown",
                    *standard_plugin_features(category),
                ],
                environment,
            )
        )
    args.output.mkdir(parents=True)
    files = [copy_file(ROOT / "LICENSE.md", args.output, Path("LICENSE.md"))]
    for source, destination in standard_host_documents(ROOT):
        files.append(copy_file(source, args.output, destination))
    for source, destination in HOST_ASSETS:
        files.append(copy_file(ROOT / source, args.output, Path(destination)))
    for sound in sorted((ROOT / "assets/sounds").iterdir()):
        if sound.is_file():
            files.append(copy_file(sound, args.output, Path("sounds") / sound.name))
    plugin_names = dict(
        re.findall(
            r'\(\s*"([a-z0-9-]+)"\s*,\s*"([^"]+)"',
            (ROOT / "crates/herbert-plugins/src/catalog.rs").read_text(),
        )
    )
    for item in guests:
        if "bin" not in item["target"]["kind"]:
            continue
        for filename in item["filenames"]:
            module = Path(filename)
            if module.suffix == ".wasm":
                name = plugin_names.get(module.stem, module.stem)
                relative = Path("plugins") / name / (name + ".wasm")
                files.append(copy_file(module, args.output, relative))
                # Plugin-owned native exports and their ordinary sibling manifest.
                assets = ROOT / "crates/herbert-plugins/packages" / module.stem
                if assets.is_dir():
                    for asset in sorted(assets.rglob("*")):
                        if asset.is_file():
                            files.append(
                                copy_file(
                                    asset,
                                    args.output,
                                    relative.parent / asset.relative_to(assets),
                                )
                            )
    if managed:
        directory, companion_manifest = managed
        relative = Path("companions/managed-decompiler") / companion_manifest["targetRid"]
        for name in ("companion.json", companion_manifest["executable"]):
            files.append(copy_file(directory / name, args.output, relative / name))
        for asset in sorted((ROOT / "helpers/compiled-functions").glob("*.efxj")):
            files.append(copy_file(asset, args.output, Path("plugins/Functions/artifacts") / asset.name))
        files.append(copy_file(ROOT / "helpers/compiled-functions/plugin.json", args.output, Path("plugins/Functions/plugin.json")))
        notice_root = ROOT / "companions/managed-decompiler/notices"
        notice_inventory = json.loads((notice_root / "dependencies.json").read_text(encoding="utf-8"))
        for notice in notice_inventory["files"]:
            path = notice_root / notice["path"]
            if Path(notice["path"]).name != notice["path"] or path.is_symlink() or file_record(path, notice_root) != notice:
                raise RuntimeError("A pinned managed-decompiler notice differs from its inventory.")
            files.append(copy_file(path, args.output, Path("third-party/managed-decompiler") / path.name))
        for name in ("dependencies.json", "ENGINE.json"):
            path = notice_root / name if name == "dependencies.json" else notice_root.parent / name
            files.append(copy_file(path, args.output, Path("third-party/managed-decompiler") / name))
    if args.ordinary_bpm_compiled:
        for asset in sorted((ROOT / "helpers/compiled-bpm").glob("*.efxj")):
            files.append(copy_file(asset, args.output, Path("plugins/BPM directives/artifacts") / asset.name))
        files.append(copy_file(ROOT / "helpers/compiled-bpm/plugin.json", args.output, Path("plugins/BPM directives/plugin.json")))
    if args.ubaq_compiled:
        for asset in sorted((ROOT / "helpers/compiled-ubaq").glob("*.efxj")):
            files.append(copy_file(asset, args.output, Path("plugins/BAQs and UBAQs/artifacts") / asset.name))
        files.append(copy_file(ROOT / "helpers/compiled-ubaq/plugin.json", args.output, Path("plugins/BAQs and UBAQs/plugin.json")))
    # Apply the tested size profile only to the native release host.
    # Plugin compilation above retains its own tested profile.
    host_environment = native_compiler_environment(native_target)
    if not args.debug:
        host_environment.update(
            CARGO_PROFILE_RELEASE_OPT_LEVEL="z",
            CARGO_PROFILE_RELEASE_LTO="fat",
            CARGO_PROFILE_RELEASE_CODEGEN_UNITS="1",
        )
    host = cargo_artifacts(
        args.cargo,
        ["--package", "herbert", "--bin", "herbert", *profile, *target, "--features", "function-compiled"],
        host_environment,
    )
    executable = executable_from(host, "herbert")
    verify_compiler_paths(executable, host_environment)
    files.append(copy_file(executable, args.output, Path(executable.name)))
    # Native builds only: a cross-target host cannot run here to compile its guests.
    precompiled = {} if args.target else precompile_guests(executable, args.output, files, compiler_cache() / "precompile-cache")
    files.extend(
        write_inventory(
            ROOT,
            args.output,
            args.cargo,
            native_target,
            None,
            False,
            {"nativeHost": host, "wasmPlugins": guests},
        )
    )
    manifest = {
        "build": "development" if args.debug else "release",
        "nativeTarget": native_target,
        "privateHelper": False,
        "pluginCategories": list(RELEASE_CATEGORIES),
        **({"managedDecompilation": {"protocolVersion": 1, "engineId": companion_manifest["engineId"], "targetRid": companion_manifest["targetRid"], "sha256": companion_manifest["sha256"], "firstUseDownload": False}} if managed else {}),
        **({"precompiledPlugins": precompiled} if precompiled else {}),
        "files": final_file_records(args.output, files),
    }
    (args.output / "package.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Built Herbert in {args.output}")


if __name__ == "__main__":
    run(main)
