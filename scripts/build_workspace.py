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
    # Neutral per-user default; set HERBERT_BUILD_HOME to keep builds elsewhere.
    cache = os.environ.get("XDG_CACHE_HOME")
    return (Path(cache) if cache else Path.home() / ".cache") / "herbert"


def compiler_cache() -> Path:
    # Retain explicitly selected isolated caches. The default is outside source.
    return Path(os.environ.get("CARGO_TARGET_DIR") or workspace()).resolve()


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
    # One canonical build environment: an inherited CARGO_INCREMENTAL=1 would override the profile and
    # refill incremental caches (docs/BUILD-STORAGE.md).
    os.environ["CARGO_INCREMENTAL"] = "0"
    if any(arg == "--output" or arg.startswith("--output=") for arg in sys.argv[1:]):
        # Explicit release/evidence destinations keep the existing no-overwrite contract.
        result = entrypoint()
        prune_after_build()
        return result
    original = sys.argv[:]
    home = workspace() / "packages"
    try:
        with candidate(home) as output:
            sys.argv.extend(["--output", str(output)])
            entrypoint()
    finally:
        sys.argv[:] = original
    print(f"Current development candidate: {home / 'current'}")
    prune_after_build()


def prune_after_build() -> None:
    # Automatic cache prune after every build (docs/BUILD-STORAGE.md).
    from prune_build_cache import prune, threshold
    report = prune(Path(os.environ["CARGO_TARGET_DIR"]), limit=threshold())
    print("Build cache: " + json.dumps({k: report[k] for k in ("bytesBefore", "bytesAfter", "thresholdExceeded", "locksHeld")}))


def cargo_config(root: Path) -> Path:
    # Optional local setup for bare cargo commands. Never overwrite authored config.
    config = root / ".cargo/config.toml"
    routing = "[build]\ntarget-dir = " + json.dumps(compiler_cache().as_posix()) + "\n"
    text = "# Local generated cache routing.\n" + routing
    # Earlier versions of this script wrote the older comment, with platform line endings.
    generated = {text, "# Local generated cache routing; see docs/BUILD-STORAGE.md.\n" + routing}
    if config.exists() and config.read_text(encoding="utf-8").replace("\r\n", "\n") not in generated:
        raise SystemExit(f"Preserving existing Cargo configuration: {config}")
    config.parent.mkdir(exist_ok=True)
    config.write_bytes(text.encode("utf-8"))
    return config


if __name__ == "__main__":
    cargo_config(Path(__file__).resolve().parents[1])
    print(f"Cargo cache: {compiler_cache()}")
