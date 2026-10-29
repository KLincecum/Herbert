"""Keep the external compiler cache bounded. Every settle runs `--settle` after its builds to keep the compiler
cache bounded; `--wipe` reclaims all compiler output on demand (see docs/BUILD-STORAGE.md).

Why: on October 28, 2026 the shared build home grew to 130 GB and the 2 TB system disk reached 0 bytes free, failing
every build. Cargo never removes superseded artifacts: each change of Cargo.lock, features, flags or profile settings
leaves another hashed copy of every affected crate in <profile>/deps.

What it does:
1. Refuses to touch the cache while any build holds it. Every Cargo build holds an exclusive lock on
   <profile dir>/.cargo-lock for its whole duration; each lock is probed without waiting.
2. Removes every incremental/ folder (incremental compilation is off for all profiles; leftovers are waste).
3. With --settle, also removes all of Cargo's output when it is still larger than the threshold; with --wipe,
   removes it regardless. It all regenerates; the next build is a cold build.
Packages, evidence and folders that are not Cargo output of this cache are never touched.

The threshold is a tunable operating default, not a hard limit: HERBERT_BUILD_CACHE_PRUNE_BYTES overrides it.

Usage: python scripts/prune_build_cache.py [--settle | --wipe] [--dry-run]. Prints one JSON object (with the sizes)
for the settle receipt."""

import json
import os
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_workspace import compiler_cache, workspace  # noqa: E402

# Generous fixed default; the unbounded cache previously reached 114 GB and filled the disk.
DEFAULT_THRESHOLD = 40 * 1000**3
# Cargo's own output in a target directory: host profile folders and per-target folders.
PROFILE_FOLDERS = ("debug", "release")
# Target-triple folders start with an architecture. Other folders in the build home (packages, evidence,
# isolated lane caches) are never Cargo output of this cache, even if they contain a profile folder.
ARCHITECTURES = ("x86_64", "i686", "aarch64", "arm", "armv7", "thumbv7", "wasm32", "wasm64", "riscv64", "powerpc64", "s390x", "loongarch64")


def target_triple(name: str) -> bool:
    parts = name.split("-")
    return len(parts) >= 3 and parts[0] in ARCHITECTURES


def size(path: Path) -> int:
    total = 0
    for root, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.path.getsize(os.path.join(root, name))
            except OSError:
                pass
    return total


def cargo_outputs(cache: Path) -> list:
    """Top-level folders Cargo created: the host profiles and <target-triple>/ folders that contain a profile."""
    found = []
    for entry in sorted(cache.iterdir()) if cache.is_dir() else []:
        if not entry.is_dir() or entry.is_symlink() or entry.is_junction():
            continue
        if entry.name in PROFILE_FOLDERS or target_triple(entry.name) and any((entry / p / ".cargo-lock").exists() for p in PROFILE_FOLDERS):
            found.append(entry)
    return found


def lock_held(lock: Path) -> bool:
    """True if another process holds this Cargo build lock."""
    try:
        stream = open(lock, "r+b")
    except PermissionError:
        return True
    except FileNotFoundError:
        return False
    with stream:
        if os.name == "nt":
            import msvcrt
            try:
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError:
                return True
            stream.seek(0)
            msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            return False
        import fcntl
        try:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return True
        fcntl.flock(stream, fcntl.LOCK_UN)
        return False


def threshold() -> int:
    value = os.environ.get("HERBERT_BUILD_CACHE_PRUNE_BYTES")
    return int(value) if value else DEFAULT_THRESHOLD


def prune(cache: Path, wipe: bool = False, dry_run: bool = False, home: Path = None, limit: int = None) -> dict:
    """limit: wipe all Cargo output if it is still larger than this after removing incremental/ (the settle mode)."""
    outputs = cargo_outputs(cache)
    locks = [lock for output in outputs for lock in output.rglob(".cargo-lock")]
    held = [str(lock) for lock in locks if lock_held(lock)]
    report = {"utc": datetime.now(timezone.utc).isoformat(), "cache": str(cache), "outputs": [o.name for o in outputs],
              "bytesBefore": sum(size(o) for o in outputs), "locksHeld": held, "removedIncremental": [],
              "wipe": wipe, "thresholdBytes": limit, "dryRun": dry_run}
    if home is not None:
        report["buildHome"] = str(home)
        report["buildHomeBytesBefore"] = size(home) if home.is_dir() else 0
    if held:
        report["skipped"] = "A build holds this compiler cache; nothing was removed."
    else:
        for output in outputs:
            for folder in sorted(output.rglob("incremental")):
                if folder.is_dir() and not folder.is_symlink():
                    report["removedIncremental"].append(str(folder.relative_to(cache)))
                    if not dry_run:
                        shutil.rmtree(folder)
        remaining = report["bytesBefore"] if dry_run else sum(size(o) for o in outputs)
        report["thresholdExceeded"] = limit is not None and remaining > limit
        if (wipe or report["thresholdExceeded"]) and not dry_run:
            for output in outputs:
                shutil.rmtree(output)
    report["bytesAfter"] = sum(size(o) for o in outputs if o.exists())
    if home is not None:
        report["buildHomeBytesAfter"] = size(home) if home.is_dir() else 0
    return report


if __name__ == "__main__":
    arguments = set(sys.argv[1:])
    unknown = arguments - {"--settle", "--wipe", "--dry-run"}
    if unknown or {"--settle", "--wipe"} <= arguments:
        raise SystemExit("Use --settle or --wipe (not both), optionally with --dry-run.")
    limit = threshold() if "--settle" in arguments else None
    print(json.dumps(prune(compiler_cache(), "--wipe" in arguments, "--dry-run" in arguments, workspace(), limit), indent=1))
