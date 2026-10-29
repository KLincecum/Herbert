"""Check child-only path mapping and preserve caller compiler settings."""

import os
import pathlib
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import build


class BuildEnvironmentTests(unittest.TestCase):
    def test_imported_compiler_helper_routes_and_sanitizes_external_cache(self):
        with tempfile.TemporaryDirectory() as temporary:
            with patch.dict(
                os.environ,
                {"HERBERT_BUILD_HOME": temporary, "CARGO_HOME": temporary},
                clear=True,
            ):
                before = dict(os.environ)
                environment = build.compiler_environment()
                self.assertEqual(dict(os.environ), before)
                cache = pathlib.Path(temporary).resolve()
                self.assertEqual(pathlib.Path(environment["CARGO_TARGET_DIR"]), cache)
                self.assertIn(
                    f"--remap-path-prefix={cache}=/build",
                    environment["CARGO_ENCODED_RUSTFLAGS"],
                )

    def test_msvc_mapping_preserves_flags_and_does_not_change_parent_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            cargo = pathlib.Path(temporary) / "cargo cache"
            with patch.dict(
                os.environ,
                {
                    "CARGO_HOME": str(cargo),
                    "_CL_": "/DPRESERVED=1",
                    "CFLAGS": "/DUSER=1",
                    "CARGO_ENCODED_RUSTFLAGS": "--cfg\x1fexisting",
                    "HERBERT_BUILD_HOME": temporary,
                },
                clear=True,
            ):
                before = dict(os.environ)
                environment = build.native_compiler_environment(
                    "x86_64-pc-windows-msvc"
                )
                self.assertEqual(dict(os.environ), before)
                expected = subprocess.list2cmdline(
                    [
                        "/experimental:deterministic",
                        f"/pathmap:{cargo.resolve()}=/cargo",
                    ]
                )
                self.assertEqual(environment["_CL_"], "/DPRESERVED=1 " + expected)
                self.assertEqual(
                    environment["CFLAGS"], "/DUSER=1 /experimental:deterministic"
                )
                self.assertTrue(
                    environment["CARGO_ENCODED_RUSTFLAGS"].startswith(
                        "--cfg\x1fexisting\x1f"
                    )
                )

    def test_non_msvc_compiler_flags_remain_unchanged(self):
        with patch.dict(
            os.environ,
            {
                "_CL_": "caller flags",
                "CFLAGS": "-DUSER=1",
                "CARGO_HOME": str(build.ROOT / "fixture-cache"),
                "HERBERT_BUILD_HOME": str(build.ROOT / "fixture-build"),
            },
            clear=True,
        ):
            environment = build.native_compiler_environment("x86_64-unknown-linux-gnu")
            self.assertEqual(environment["_CL_"], "caller flags")
            self.assertEqual(environment["CFLAGS"], "-DUSER=1")

    def test_native_binary_check_catches_wide_paths_and_accepts_generic_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            executable = root / "fixture.bin"
            environment = {"CARGO_HOME": str(root / "cache")}
            executable.write_bytes(str(root / "cache" / "header.h").encode("utf-16le"))
            with self.assertRaisesRegex(RuntimeError, "retains a local build path"):
                build.verify_compiler_paths(executable, environment)
            executable.write_bytes(b"/cargo/header.h")
            build.verify_compiler_paths(executable, environment)


if __name__ == "__main__":
    unittest.main()
