"""Keep compiler caches external and replace only unchanged, tool-owned candidates."""

from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile

MARKER = ".herbert-build.json"


def workspace() -> Path:
    override = os.environ.get("HERBERT_BUILD_HOME")
    if override:
        return Path(override).expanduser().resolve()
    workbench = Path("C:/Tools/Workbench/workspace/build/hoover")
    if (
        os.name == "nt"
        and Path("C:/Tools/Workbench/manifests/installed.json").is_file()
    ):
        return workbench
    return (
        Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))) / "herbert"
    )


def compiler_cache() -> Path:
    # Retain explicitly selected isolated caches. The default is outside source.
    return Path(os.environ.get("CARGO_TARGET_DIR", str(workspace()))).resolve()


def inventory(folder: Path) -> dict:
    result = {}
    for path in sorted(folder.rglob("*")):
        if path.is_symlink() or path.is_junction():
            raise RuntimeError(f"Managed candidates cannot contain links: {path}")
        if path.is_file() and path != folder / MARKER:
            with path.open("rb") as stream:
                result[path.relative_to(folder).as_posix()] = hashlib.file_digest(
                    stream, "sha256"
                ).hexdigest()
    return result


def verify_owned(folder: Path) -> None:
    if folder.is_symlink() or folder.is_junction():
        raise RuntimeError(f"Refusing linked candidate: {folder}")
    if folder.exists():
        marker = folder / MARKER
        if not marker.is_file():
            raise RuntimeError(
                f"Preserving unowned output: {folder}. Use --output for a new folder."
            )
        data = json.loads(marker.read_text(encoding="utf-8"))
        if data.get("kind") != "herbert-build-candidate-v1" or data.get(
            "files"
        ) != inventory(folder):
            raise RuntimeError(
                f"Preserving modified candidate: {folder}. It may contain plugin settings "
                "or user files. Use --output for a new folder."
            )


@contextmanager
def candidate(home: Path):
    """Keep the previous successful build until its replacement finishes."""
    home = home.resolve()
    home.mkdir(parents=True, exist_ok=True)
    current = home / "current"
    lock = home / ".build-lock"
    try:
        lock.mkdir()
    except FileExistsError as error:
        raise RuntimeError(
            f"Candidate build already active or interrupted: {lock}"
        ) from error
    try:
        verify_owned(current)
        with tempfile.TemporaryDirectory(prefix="building-", dir=home) as temporary:
            staged = Path(temporary) / "package"
            yield staged
            if not staged.is_dir():
                raise RuntimeError("The build did not produce its candidate.")
            (staged / MARKER).write_text(
                json.dumps(
                    {"kind": "herbert-build-candidate-v1", "files": inventory(staged)},
                    indent=2,
                ),
                encoding="utf-8",
            )
            # Recheck immediately before promotion: never erase settings changed during a build.
            verify_owned(current)
            previous = Path(temporary) / "previous"
            if current.exists():
                current.rename(previous)
            try:
                staged.rename(current)
            except BaseException:
                if previous.exists():
                    previous.rename(current)
                raise
            # Only the verified, tool-owned previous candidate is discarded.
    finally:
        lock.rmdir()


def run(entrypoint):
    if "--help" in sys.argv or "-h" in sys.argv:
        return entrypoint()
    os.environ.setdefault("CARGO_TARGET_DIR", str(compiler_cache()))
    if any(arg == "--output" or arg.startswith("--output=") for arg in sys.argv[1:]):
        # Explicit release/evidence destinations keep the existing no-overwrite contract.
        return entrypoint()
    original = sys.argv[:]
    home = workspace() / "packages"
    try:
        with candidate(home) as output:
            sys.argv.extend(["--output", str(output)])
            entrypoint()
    finally:
        sys.argv[:] = original
    print(f"Current development candidate: {home / 'current'}")


if __name__ == "__main__":
    # Optional local setup for bare cargo commands. Never overwrite authored config.
    root = Path(__file__).resolve().parents[1]
    config = root / ".cargo/config.toml"
    text = (
        "# Local generated cache routing.\n[build]\ntarget-dir = "
        + json.dumps(compiler_cache().as_posix())
        + "\n"
    )
    if config.exists() and config.read_text(encoding="utf-8") != text:
        raise SystemExit(f"Preserving existing Cargo configuration: {config}")
    config.parent.mkdir(exist_ok=True)
    config.write_text(text, encoding="utf-8")
    print(f"Cargo cache: {compiler_cache()}")
