"""Exercise replacement, failure recovery and preservation of user-owned files."""

from pathlib import Path
import tempfile
import unittest
import os
import sys
from unittest.mock import patch
from build_workspace import candidate, run


class CandidateTests(unittest.TestCase):
    def test_success_replaces_only_unchanged_candidate(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            for value in ("first", "second"):
                with candidate(home) as stage:
                    stage.mkdir()
                    (stage / "host.bin").write_text(value)
            self.assertEqual((home / "current/host.bin").read_text(), "second")
            self.assertEqual([p.name for p in home.iterdir()], ["current"])

    def test_failure_keeps_previous_and_removes_partial_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            with candidate(home) as stage:
                stage.mkdir()
                (stage / "host.bin").write_text("working")
            with self.assertRaisesRegex(RuntimeError, "compiler failed"):
                with candidate(home) as stage:
                    stage.mkdir()
                    (stage / "partial.bin").write_text("partial")
                    raise RuntimeError("compiler failed")
            self.assertEqual((home / "current/host.bin").read_text(), "working")
            self.assertEqual([p.name for p in home.iterdir()], ["current"])

    def test_modified_or_added_user_file_blocks_replacement(self):
        for modified in (False, True):
            with self.subTest(modified=modified), tempfile.TemporaryDirectory() as tmp:
                home = Path(tmp)
                with candidate(home) as stage:
                    stage.mkdir()
                    (stage / "host.bin").write_text("working")
                user = (
                    home / "current" / ("host.bin" if modified else "user.state.json")
                )
                user.write_text("must survive")
                with self.assertRaisesRegex(RuntimeError, "modified candidate"):
                    with candidate(home):
                        self.fail("Must refuse before building.")
                self.assertEqual(user.read_text(), "must survive")

    def test_unowned_directory_is_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "current").mkdir()
            with self.assertRaisesRegex(RuntimeError, "unowned output"):
                with candidate(home):
                    self.fail("Unowned folder must be preserved.")
            self.assertTrue((home / "current").exists())

    def test_change_during_build_is_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            with candidate(home) as stage:
                stage.mkdir()
                (stage / "host.bin").write_text("working")
            with self.assertRaisesRegex(RuntimeError, "modified candidate"):
                with candidate(home) as stage:
                    stage.mkdir()
                    (stage / "host.bin").write_text("new")
                    (home / "current/user.state.json").write_text("keep")
            self.assertEqual((home / "current/host.bin").read_text(), "working")
            self.assertEqual((home / "current/user.state.json").read_text(), "keep")

    def test_concurrent_build_is_refused_without_stealing_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            with candidate(home) as stage:
                with self.assertRaisesRegex(RuntimeError, "already active"):
                    with candidate(home):
                        self.fail("Must not share candidate output.")
                self.assertTrue((home / ".build-lock").is_dir())
                stage.mkdir()
                (stage / "host.bin").write_text("working")

    def test_default_cli_routes_cache_and_promotes_built_package(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)

            def build():
                self.assertEqual(Path(os.environ["CARGO_TARGET_DIR"]), home.resolve())
                output = Path(sys.argv[sys.argv.index("--output") + 1])
                self.assertFalse(output.exists())
                output.mkdir()
                (output / "host.bin").write_text("built")

            with patch.dict(os.environ, {"HERBERT_BUILD_HOME": tmp}, clear=True):
                with patch.object(sys, "argv", ["build.py", "--debug"]):
                    run(build)
                    self.assertEqual(sys.argv, ["build.py", "--debug"])
            self.assertEqual((home / "packages/current/host.bin").read_text(), "built")

    def test_explicit_output_and_cache_selection_are_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            args = ["build.py", "--output", str(home / "release")]

            def build():
                self.assertEqual(sys.argv, args)
                self.assertEqual(os.environ["CARGO_TARGET_DIR"], str(home / "isolated"))

            with patch.dict(
                os.environ,
                {"HERBERT_BUILD_HOME": tmp, "CARGO_TARGET_DIR": str(home / "isolated")},
                clear=True,
            ):
                with patch.object(sys, "argv", args[:]):
                    run(build)
            self.assertFalse((home / "packages").exists())


if __name__ == "__main__":
    unittest.main()
