"""Exercise replacement, failure recovery and preservation of user-owned files."""

from pathlib import Path
import tempfile
import unittest
import os
import sys
from unittest.mock import patch
from build_workspace import candidate, cargo_config, run, workspace


class CandidateTests(unittest.TestCase):
    def test_cargo_config_upgrades_generated_text_and_preserves_authored(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            cache = (root / "cache").resolve().as_posix()
            routing = '[build]\ntarget-dir = "' + cache + '"\n'
            config = root / ".cargo/config.toml"
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": cache}, clear=True):
                config.parent.mkdir()
                config.write_bytes(("# Local generated cache routing; see docs/BUILD-STORAGE.md.\n" + routing).replace("\n", "\r\n").encode())
                cargo_config(root)
                self.assertEqual(config.read_bytes(), ("# Local generated cache routing.\n" + routing).encode())
                cargo_config(root)
                authored = b'[build]\ntarget-dir = "elsewhere"\n'
                config.write_bytes(authored)
                with self.assertRaises(SystemExit):
                    cargo_config(root)
                self.assertEqual(config.read_bytes(), authored)

    def test_workspace_defaults_have_no_machine_path(self):
        with tempfile.TemporaryDirectory() as tmp:
            with patch.dict(os.environ, {"XDG_CACHE_HOME": tmp}, clear=True):
                self.assertEqual(workspace(), Path(tmp) / "herbert")
            with patch.dict(os.environ, {"HERBERT_BUILD_HOME": tmp}, clear=True):
                self.assertEqual(workspace(), Path(tmp).resolve())

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

    def test_builds_never_use_incremental_compilation(self):
        with tempfile.TemporaryDirectory() as tmp:
            seen = []
            with patch.dict(os.environ, {"HERBERT_BUILD_HOME": tmp, "CARGO_INCREMENTAL": "1"}, clear=True):
                for args in (["build.py", "--debug"], ["build.py", "--output", str(Path(tmp) / "release")]):
                    with patch.object(sys, "argv", args):
                        run(lambda: seen.append(os.environ["CARGO_INCREMENTAL"]) or Path(sys.argv[-1]).mkdir())
            self.assertEqual(seen, ["0", "0"])


if __name__ == "__main__":
    unittest.main()
