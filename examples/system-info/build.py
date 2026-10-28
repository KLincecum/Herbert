"""Build the two teaching bundles without a shell or implicit install hooks."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parent


def run(command: list[str], directory: Path) -> None:
    subprocess.run(command, cwd=directory, check=True, shell=False)


def copy_bundle(wasm: Path, state: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(wasm, destination / "system-info.wasm")
    # Preserve a config that the developer has already edited or enabled.
    target = destination / "system-info.state.json"
    if not target.exists():
        shutil.copyfile(state, target)


def build_rust(cargo: str, output: Path) -> None:
    source = ROOT / "rust"
    result = subprocess.run(
        [
            cargo,
            "build",
            "--manifest-path",
            str(source / "Cargo.toml"),
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--locked",
            "--message-format=json",
        ],
        cwd=source,
        check=True,
        capture_output=True,
        text=True,
        shell=False,
    )
    # Use this build's compiler output. Never copy a stale module from a cache.
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact":
            continue
        if message["target"]["name"] != "herbert_system_info_rust":
            continue
        for filename in message["filenames"]:
            module = Path(filename)
            if module.suffix == ".wasm":
                copy_bundle(module, source / "system-info.state.json", output / "rust")
                return
    raise RuntimeError("Cargo completed without producing the example module.")


def build_assemblyscript(node: str, source: Path, output: Path) -> None:
    # Install pinned dependencies with npm ci --ignore-scripts before this step.
    # A prepared source copy can be supplied with --assemblyscript-source.
    run([node, str(source / "build.mjs")], source)
    copy_bundle(
        source / "build/system-info.wasm",
        ROOT / "assemblyscript/system-info.state.json",
        output / "assemblyscript",
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo", default="cargo", help="The Cargo executable.")
    parser.add_argument("--node", default="node", help="The Node executable.")
    parser.add_argument(
        "--output", type=Path, required=True, help="The bundle directory."
    )
    parser.add_argument(
        "--assemblyscript-source",
        type=Path,
        default=ROOT / "assemblyscript",
        help="The source directory with pinned dependencies installed.",
    )
    args = parser.parse_args()
    build_rust(args.cargo, args.output)
    build_assemblyscript(args.node, args.assemblyscript_source, args.output)
    print(f"Built both teaching bundles in {args.output}")


if __name__ == "__main__":
    main()
