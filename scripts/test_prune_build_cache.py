import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import prune_build_cache as prune_module  # noqa: E402


def write(path: Path, size: int = 10) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"x" * size)


class PruneBuildCacheTest(unittest.TestCase):
    def cache(self, root: Path) -> Path:
        write(root / "debug/.cargo-lock", 0)
        write(root / "debug/deps/libherbert-aaaa.rlib", 1000)
        write(root / "debug/incremental/herbert-x/s-1/query-cache.bin", 500)
        write(root / "wasm32-unknown-unknown/release/.cargo-lock", 0)
        write(root / "wasm32-unknown-unknown/release/deps/herbert_plugins-bbbb.wasm", 300)
        write(root / "packages/current/herbert.exe", 700)
        write(root / "c10-oracle-obj/debug/.cargo-lock", 0)
        write(root / "c10-oracle-obj/debug/deps/oracle.rlib", 400)
        return root

    def test_default_removes_only_incremental_and_reports_sizes(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = self.cache(Path(tmp))
            report = prune_module.prune(cache, home=cache)
            self.assertEqual(report["outputs"], ["debug", "wasm32-unknown-unknown"])
            self.assertFalse((cache / "debug/incremental").exists())
            self.assertTrue((cache / "debug/deps/libherbert-aaaa.rlib").exists())
            self.assertEqual((report["bytesBefore"], report["bytesAfter"]), (1800, 1300))
            self.assertEqual((report["buildHomeBytesBefore"], report["buildHomeBytesAfter"]), (2900, 2400))

    def test_wipe_removes_only_cargo_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = self.cache(Path(tmp))
            report = prune_module.prune(cache, wipe=True)
            self.assertFalse((cache / "debug").exists())
            self.assertFalse((cache / "wasm32-unknown-unknown").exists())
            self.assertTrue((cache / "packages/current/herbert.exe").exists(), "packages are never Cargo output")
            self.assertTrue((cache / "c10-oracle-obj/debug/deps/oracle.rlib").exists(), "other caches in the build home are kept")
            self.assertEqual(report["bytesAfter"], 0)

    def test_settle_keeps_output_under_threshold_and_wipes_above_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = self.cache(Path(tmp))
            kept = prune_module.prune(cache, limit=1300)
            self.assertFalse(kept["thresholdExceeded"], "incremental/ is removed first; 1300 bytes remain")
            self.assertTrue((cache / "debug/deps/libherbert-aaaa.rlib").exists())
            self.assertFalse((cache / "debug/incremental").exists())
            wiped = prune_module.prune(cache, limit=1299)
            self.assertTrue(wiped["thresholdExceeded"])
            self.assertFalse((cache / "debug").exists())
            self.assertTrue((cache / "packages/current/herbert.exe").exists())
            self.assertTrue((cache / "c10-oracle-obj/debug/deps/oracle.rlib").exists())

    def test_threshold_override_and_default(self):
        old = os.environ.pop("HERBERT_BUILD_CACHE_PRUNE_BYTES", None)
        try:
            self.assertEqual(prune_module.threshold(), prune_module.DEFAULT_THRESHOLD)
            os.environ["HERBERT_BUILD_CACHE_PRUNE_BYTES"] = "123"
            self.assertEqual(prune_module.threshold(), 123)
        finally:
            os.environ.pop("HERBERT_BUILD_CACHE_PRUNE_BYTES", None)
            if old is not None:
                os.environ["HERBERT_BUILD_CACHE_PRUNE_BYTES"] = old

    def test_dry_run_changes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = self.cache(Path(tmp))
            report = prune_module.prune(cache, wipe=True, dry_run=True)
            self.assertEqual(report["removedIncremental"], [os.path.join("debug", "incremental")])
            self.assertTrue((cache / "debug/incremental").exists())
            self.assertTrue((cache / "debug/deps/libherbert-aaaa.rlib").exists())

    def test_held_build_lock_skips_everything(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = self.cache(Path(tmp))
            with open(cache / "wasm32-unknown-unknown/release/.cargo-lock", "r+b") as holder:
                if os.name == "nt":
                    import msvcrt
                    msvcrt.locking(holder.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
                report = prune_module.prune(cache, wipe=True)
                self.assertEqual(len(report["locksHeld"]), 1)
                self.assertIn("skipped", report)
                self.assertTrue((cache / "debug/incremental").exists())
                self.assertTrue((cache / "debug/deps/libherbert-aaaa.rlib").exists())
                if os.name == "nt":
                    holder.seek(0)
                    msvcrt.locking(holder.fileno(), msvcrt.LK_UNLCK, 1)


if __name__ == "__main__":
    unittest.main()
