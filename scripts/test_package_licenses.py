import importlib.util, json, os, pathlib, subprocess, tempfile, unittest, zipfile, hashlib
from unittest.mock import patch

SOURCE = pathlib.Path(__file__).with_name("package_licenses.py")
spec = importlib.util.spec_from_file_location("package_licenses", SOURCE)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class InventoryTests(unittest.TestCase):
    def test_extended_path_reads_long_filename_and_rejects_sibling_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = module.filesystem_path(pathlib.Path(temporary) / ("fixture" * 16) / ("source" * 16))
            root.mkdir(parents=True)
            file = root / "notice.txt"
            file.write_bytes(b"exact upstream text")
            self.assertEqual(file.resolve(strict=True).read_bytes(), b"exact upstream text")
            self.assertTrue(file.resolve(strict=True).is_relative_to(root.resolve()))
            sibling = root.parent / "outside.txt"
            sibling.write_bytes(b"outside")
            self.assertFalse(sibling.resolve(strict=True).is_relative_to(root.resolve()))
            file.unlink()
            sibling.unlink()
            root.rmdir()
            root.parent.rmdir()

    def test_supplement_requires_exact_package_and_text_integrity(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            directory = root / "assets/third-party-supplemental"
            directory.mkdir(parents=True)
            data = b"pinned upstream copyright\r\n"
            (directory / "LICENSE").write_bytes(data)
            text = {
                "path": "LICENSE",
                "sourcePath": "LICENSE",
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "url": "https://example.invalid/pinned/LICENSE",
            }
            match = {
                "ecosystem": "cargo",
                "name": "fixture",
                "version": "1",
                "cargoLockChecksum": "lock",
                "manifestSha256": "manifest",
                "commit": "revision",
                "resolution": "pinned",
                "texts": [text],
            }
            (directory / "manifest.json").write_text(json.dumps({"packages": [match]}))

            def record():
                return {
                    "ecosystem": "cargo",
                    "name": "fixture",
                    "version": "1",
                    "cargoLockChecksum": "lock",
                    "manifestSha256": "manifest",
                    "source": "registry",
                    "texts": [],
                    "issues": ["missing locally"],
                }

            r = record()
            module.supplement_records(root, root / "out", [r])
            self.assertEqual(r["issues"], [])
            self.assertEqual(r["observedSourceIssues"], ["missing locally"])
            self.assertEqual((root / "out" / r["texts"][0]["path"]).read_bytes(), data)
            drift = record()
            drift["cargoLockChecksum"] = "other"
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                module.supplement_records(root, root / "drift", [drift])
            (directory / "LICENSE").write_bytes(b"tampered")
            with self.assertRaisesRegex(RuntimeError, "integrity failed"):
                module.supplement_records(root, root / "bad", [record()])

    def test_normal_edges_exclude_build_dev_and_macro_paths(self):
        def p(name, kind="lib"):
            return {"id": name, "name": name, "targets": [{"kind": [kind]}]}

        def d(name, kind=None):
            return {"pkg": name, "dep_kinds": [{"kind": kind}]}

        metadata = {
            "packages": [
                p("herbert"),
                p("runtime"),
                p("build"),
                p("test"),
                p("macro", "proc-macro"),
                p("macro-child"),
            ],
            "workspace_members": ["herbert"],
            "resolve": {
                "nodes": [
                    {
                        "id": "herbert",
                        "deps": [
                            d("runtime"),
                            d("build", "build"),
                            d("test", "dev"),
                            d("macro"),
                        ],
                    },
                    {"id": "runtime", "deps": []},
                    {"id": "macro", "deps": [d("macro-child")]},
                ]
            },
        }
        self.assertEqual(
            module.runtime_closure(metadata, "herbert"), {"herbert", "runtime"}
        )

    def test_nested_texts_are_verbatim_and_missing_declarations_stay_visible(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / "source"
            source.mkdir()
            out = root / "output"
            (source / "LICENSE").write_bytes(b"Copyright\r\nLicense\r\n")
            (source / "vendor/licenses").mkdir(parents=True)
            (source / "vendor/licenses/Apache-2.0.txt").write_bytes(b"upstream\n")
            (source / "node_modules/other").mkdir(parents=True)
            (source / "node_modules/other/LICENSE").write_bytes(b"not this package")
            (source / "fonts").mkdir()
            (source / "fonts/OFL.txt").write_bytes(b"font license")
            (source / "fonts/Hack-Regular.txt").write_bytes(b"font copyright")
            texts, issues = module.copy_notices(source, out, "crate", "MISSING.txt")
            self.assertEqual(len(texts), 4)
            self.assertTrue(any("absent" in issue for issue in issues))
            for item in texts:
                self.assertEqual(
                    (out / item["path"]).read_bytes(),
                    (source / item["sourcePath"]).read_bytes(),
                )
                self.assertEqual(module.digest(out / item["path"]), item["sha256"])
            (root / "outside-license").write_bytes(b"outside")
            _, issues = module.notice_files(source, "../outside-license")
            self.assertTrue(any("outside" in issue for issue in issues))

    def test_declared_parent_segments_that_return_inside_source_copy_one_normalized_file(
        self,
    ):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / "source"
            source.mkdir()
            out = root / "output"
            (source / "LICENSE").write_bytes(b"exact text")
            texts, issues = module.copy_notices(
                source, out, "crate", "../source/LICENSE"
            )
            self.assertEqual(issues, [])
            self.assertEqual(len(texts), 1)
            self.assertEqual(texts[0]["sourcePath"], "LICENSE")
            self.assertEqual(
                texts[0]["path"],
                "third-party/texts/" + module.digest(source / "LICENSE") + ".txt",
            )
            self.assertEqual(
                [p.relative_to(out).as_posix() for p in out.rglob("*") if p.is_file()],
                [texts[0]["path"]],
            )

    def test_long_upstream_paths_flatten_without_losing_distinct_sources(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / "source"
            nested = source / "src/backend/xdg_desktop_portal/window_identifier"
            nested.mkdir(parents=True)
            (nested / "LICENSE-MIT").write_bytes(b"shared notice")
            (source / "LICENSE").write_bytes(b"shared notice")
            (source / "NOTICE").write_bytes(b"different notice")
            out = root / "output"
            records, issues = module.copy_notices(source, out, "package")
            self.assertEqual(issues, [])
            self.assertEqual(len(records), 3)
            self.assertEqual(len({item["path"] for item in records}), 2)
            self.assertEqual(len(list((out / "third-party/texts").glob("*"))), 2)
            for item in records:
                self.assertEqual(len(pathlib.Path(item["path"]).parts), 3)
                self.assertEqual(
                    (out / item["path"]).read_bytes(),
                    (source / item["sourcePath"]).read_bytes(),
                )

    def test_windows_junction_does_not_import_outside_notice_text(self):
        if os.name != "nt":
            self.skipTest("Windows junction fixture")
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp).resolve()
            source = root / "source"
            source.mkdir()
            outside = root / "outside"
            outside.mkdir()
            (outside / "LICENSE").write_bytes(b"outside source")
            junction = source / "linked"
            result = subprocess.run(
                [
                    str(pathlib.Path(os.environ["SystemRoot"]) / "System32/cmd.exe"),
                    "/d",
                    "/c",
                    "mklink",
                    "/J",
                    "linked",
                    "..\\outside",
                ],
                cwd=source,
                capture_output=True,
                text=True,
                shell=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            try:
                self.assertTrue(junction.is_junction())
                files, _ = module.notice_files(source)
                self.assertEqual(files, [])
                files, issues = module.notice_files(source, "linked/LICENSE")
                self.assertEqual(files, [])
                self.assertTrue(any("outside" in value for value in issues))
            finally:
                self.assertTrue(junction.parent.resolve().is_relative_to(root))
                self.assertEqual(junction.resolve(), outside)
                os.rmdir(junction)
            self.assertEqual((outside / "LICENSE").read_bytes(), b"outside source")

    def test_symlink_notice_is_not_followed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / "source"
            source.mkdir()
            outside = root / "outside"
            outside.write_bytes(b"not owned")
            try:
                (source / "LICENSE").symlink_to(outside)
            except OSError:
                self.skipTest("Creating symlinks requires host capability")
            files, issues = module.notice_files(source)
            self.assertEqual(files, [])
            self.assertTrue(issues)

    def test_npm_name_or_version_drift_is_refused_and_lock_integrity_is_retained(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            reference = root / "examples/system-info/assemblyscript"
            reference.mkdir(parents=True)
            source = root / "installed"
            source.mkdir()
            package = source / "node_modules/example"
            package.mkdir(parents=True)
            out = root / "output"
            lock = {
                "packages": {
                    "": {},
                    "node_modules/example": {
                        "version": "1.0.0",
                        "resolved": "https://example.invalid/package.tgz",
                        "integrity": "sha512-fixture",
                        "license": "MIT",
                        "dev": True,
                    },
                }
            }
            for folder in [reference, source]:
                (folder / "package-lock.json").write_text(
                    json.dumps(lock), encoding="utf8"
                )
            (package / "package.json").write_text(
                json.dumps({"name": "example", "version": "2.0.0"}), encoding="utf8"
            )
            with self.assertRaisesRegex(RuntimeError, "version differs"):
                module.npm_records(root, source, out)
            (package / "package.json").write_text(
                json.dumps({"name": "example", "version": "1.0.0"}), encoding="utf8"
            )
            (package / "LICENSE").write_bytes(b"MIT fixture")
            records = module.npm_records(root, source, out)
            self.assertEqual(records[0]["npmIntegrity"], "sha512-fixture")
            self.assertEqual(records[0]["roles"], ["buildOrFormattingToolingOnly"])
            (source / "package-lock.json").write_text("{}", encoding="utf8")
            with self.assertRaisesRegex(RuntimeError, "pinned project lock"):
                module.npm_records(root, source, out)

    def test_manifest_records_cover_inventory_and_all_notices(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / "Cargo.lock").write_bytes(b"package = []")
            assembly = root / "assembly"
            assembly.mkdir()
            (assembly / "package-lock.json").write_bytes(b"npm-lock")
            out = root / "package"
            out.mkdir()
            package = {
                "id": "herbert",
                "name": "herbert",
                "license": None,
                "license_file": "LICENSE.md",
                "manifest_path": str(root / "Cargo.toml"),
            }
            (root / "LICENSE.md").write_bytes(b"First-party license fixture")
            metadata = {"packages": [package], "workspace_members": ["herbert"]}

            def records(*args):
                file = out / "third-party/texts/example/LICENSE"
                file.parent.mkdir(parents=True)
                file.write_bytes(b"exact license")
                return []

            with (
                patch.object(module, "cargo_metadata", return_value=metadata),
                patch.object(module, "cargo_records", side_effect=records),
                patch.object(module, "npm_records", return_value=[]),
                patch.object(
                    module.subprocess,
                    "run",
                    return_value=type(
                        "Result",
                        (),
                        {"stdout": "cargo fixture\nhost: fixture-target\n"},
                    )(),
                ),
            ):
                files = module.write_uncompressed_inventory(
                    root, out, "cargo", "fixture-target", assembly, False, {}
                )
            self.assertEqual(
                {item["path"] for item in files},
                {
                    "third-party/texts/example/LICENSE",
                    "third-party/README.md",
                    "third-party/dependencies.json",
                },
            )
            data = json.loads(
                (out / "third-party/dependencies.json").read_text(encoding="utf8")
            )
            self.assertIsNone(data["productLicenseDeclaration"])
            self.assertEqual(data["productLicenseFile"], "LICENSE.md")
            for item in files:
                self.assertEqual(module.digest(out / item["path"]), item["sha256"])

    def test_workspace_license_file_preserves_bytes_and_portable_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            package_dir = root / "crates/product"
            package_dir.mkdir(parents=True)
            content = b"First-party license\r\n"
            (root / "LICENSE.md").write_bytes(content)
            package = {
                "manifest_path": str(package_dir / "Cargo.toml"),
                "license_file": "../../LICENSE.md",
            }
            output = root / "output"
            texts, issues, declared = module.copy_workspace_license(
                package, root, output
            )
            self.assertEqual(issues, [])
            self.assertEqual(declared, "LICENSE.md")
            self.assertEqual(texts[0]["sourcePath"], "LICENSE.md")
            self.assertEqual((output / texts[0]["path"]).read_bytes(), content)
            outside = root.parent / (root.name + "-outside-license")
            outside.write_bytes(b"outside")
            try:
                package["license_file"] = str(outside)
                with self.assertRaisesRegex(RuntimeError, "outside the workspace"):
                    module.copy_workspace_license(package, root, output)
            finally:
                outside.unlink()

    def test_archive_preserves_all_references_and_readable_notices(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            source = root / "source"
            source.mkdir()
            (source / "LICENSE").write_bytes(b"license\r\n\xc2\xa9")
            (source / "NOTICE").write_bytes(b"attribution\r\n")
            staged = root / "staged"
            records, _ = module.copy_notices(source, staged, "example")
            inventory = {
                "schemaVersion": 1,
                "packages": [
                    {
                        "ecosystem": "cargo",
                        "name": "example",
                        "version": "1",
                        "source": "fixture",
                        "texts": records,
                        "issues": ["retained issue"],
                    }
                ],
            }
            (staged / "third-party/dependencies.json").write_text(json.dumps(inventory))
            (staged / "third-party/README.md").write_bytes(b"package list")
            output = root / "output"
            files = module.archive_inventory(staged, output)
            self.assertEqual(len(files), 3)
            archive = output / "third-party/THIRD-PARTY-LICENSES.zip"
            with zipfile.ZipFile(archive) as zipped:
                updated = json.loads(zipped.read("dependencies.json"))
                self.assertEqual(updated["packages"][0]["issues"], ["retained issue"])
                for record in updated["packages"][0]["texts"]:
                    content = zipped.read(record["path"])
                    self.assertEqual(
                        content, (source / record["sourcePath"]).read_bytes()
                    )
                    self.assertEqual(
                        hashlib.sha256(content).hexdigest(), record["sha256"]
                    )
                    self.assertTrue(record["path"].endswith("/" + record["sourcePath"]))
            self.assertIn(
                (source / "NOTICE").read_bytes(),
                (output / "third-party/THIRD-PARTY-NOTICES.txt").read_bytes(),
            )
            other = root / "other"
            module.archive_inventory(staged, other)
            self.assertEqual(
                archive.read_bytes(),
                (other / "third-party/THIRD-PARTY-LICENSES.zip").read_bytes(),
            )
            (staged / records[0]["path"]).write_bytes(b"changed")
            with self.assertRaisesRegex(RuntimeError, "differ"):
                module.archive_inventory(staged, root / "bad")

    def test_build_inventory_stages_then_returns_only_final_distribution_files(self):
        with tempfile.TemporaryDirectory() as temp:
            output = pathlib.Path(temp) / "output"

            def stage(root, staged, *args):
                directory = staged / "third-party"
                directory.mkdir()
                (directory / "dependencies.json").write_text('{"packages": []}')
                (directory / "README.md").write_text("empty fixture inventory")
                self.assertNotEqual(staged, output)

            with patch.object(
                module, "write_uncompressed_inventory", side_effect=stage
            ):
                files = module.write_inventory(
                    pathlib.Path(temp),
                    output,
                    "cargo",
                    "target",
                    pathlib.Path(temp),
                    False,
                    {},
                )
            self.assertEqual(
                {f["path"] for f in files},
                {
                    "third-party/README.md",
                    "third-party/THIRD-PARTY-NOTICES.txt",
                    "third-party/THIRD-PARTY-LICENSES.zip",
                },
            )
            self.assertFalse((output / "third-party/texts").exists())


if __name__ == "__main__":
    unittest.main()
