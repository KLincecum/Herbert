"""Verify the explicit release boundary without deleting unfinished sources."""

import hashlib
import json
import pathlib
import posixpath
import re
from urllib.parse import unquote, urlsplit
import tempfile
import unittest
from unittest.mock import patch
import io
import build
import build_workspace
import os


class StandardPackageTests(unittest.TestCase):
    def test_only_rewritten_collectors_ship(self):
        expected = {
            "C01",
            "C02",
            "C03",
            "C04",
            "C06",
            "C08",
            "C09",
            "C10",
            "C11",
            "C12",
            "C13",
            "C14",
            "C15",
            "C16",
            "C17",
            "C18",
            "C19",
            "C20",
            "C21",
            "C22",
            "C24",
            "C25",
            "C26",
            "C27",
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            names = [category.lower() + ".rs" for category in expected] + [
                "c05.rs",
                "c99.rs",
            ]
            for name in names:
                (root / name).write_text("synthetic source", encoding="utf-8")
            self.assertEqual(
                {category for _, category in build.standard_plugins(root)}, expected
            )
            self.assertEqual(len(list(root.glob("*.rs"))), len(names))
            (root / "c04.rs").unlink()
            with self.assertRaisesRegex(RuntimeError, "c04"):
                build.standard_plugins(root)

    def test_standard_compiled_features_are_always_selected(self):
        self.assertEqual(build.standard_plugin_features("C04"), ["--features", "ubaq-compiled"])
        self.assertEqual(build.standard_plugin_features("C06"), ["--features", "ordinary-bpm-compiled"])
        self.assertEqual(build.standard_plugin_features("C08"), ["--features", "function-compiled"])
        for category in set(build.RELEASE_CATEGORIES) - {"C04", "C06", "C08"}:
            self.assertEqual(build.standard_plugin_features(category), [])

    def test_missing_companion_stops_before_compilation(self):
        with patch("sys.argv", ["build.py"]), patch("sys.stderr", new_callable=io.StringIO), patch.object(build, "cargo_artifacts") as compile_call:
            with self.assertRaises(SystemExit) as failure:
                build.main()
            self.assertEqual(failure.exception.code, 2)
            compile_call.assert_not_called()


    def test_standard_main_packages_complete_compiled_composition(self):
        self.assert_standard_package(False)

    def test_default_wrapper_packages_complete_compiled_composition(self):
        self.assert_standard_package(True)

    def assert_standard_package(self, managed_default):
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary)
            output = work / "build-home" / "packages" / "current" if managed_default else work / "package"
            companion_root = work / "companions"
            companion_dir = companion_root / "win-x64"
            companion_dir.mkdir(parents=True)
            companion_bytes = b"test native companion"
            companion_name = "herbert-managed-decompiler.exe"
            (companion_dir / companion_name).write_bytes(companion_bytes)
            companion = {
                "protocolVersion": 1,
                "engineId": "ilspy-11.1.0.9782-herbert-v1",
                "targetRid": "win-x64",
                "executable": companion_name,
                "bytes": len(companion_bytes),
                "sha256": hashlib.sha256(companion_bytes).hexdigest(),
                "cancelGraceMilliseconds": 185,
            }
            (companion_dir / "companion.json").write_text(json.dumps(companion), encoding="utf-8")
            compiler_calls = []

            def compiler(cargo, arguments, environment):
                compiler_calls.append(arguments)
                binary = arguments[arguments.index("--bin") + 1]
                is_host = binary == "herbert"
                artifact = work / (binary + (".exe" if is_host else ".wasm"))
                artifact.write_bytes(("test artifact " + binary).encode())
                return [{
                    "target": {"name": binary, "kind": ["bin"]},
                    "filenames": [str(artifact)],
                    "executable": str(artifact) if is_host else None,
                }]

            arguments = [
                "build.py",
                "--target", "x86_64-pc-windows-msvc",
                "--managed-decompilation-companions", str(companion_root),
            ]
            if not managed_default:
                arguments.extend(["--output", str(output)])
            with patch.object(build_workspace, "workspace", return_value=work / "build-home"), patch.dict(os.environ), patch("sys.argv", arguments), patch.object(build.shutil, "which", return_value="test-cargo"), patch.object(build, "compiler_environment", return_value={}), patch.object(build, "native_compiler_environment", return_value={}), patch.object(build, "cargo_artifacts", side_effect=compiler), patch.object(build, "verify_compiler_paths"), patch.object(build, "write_inventory", return_value=[]), patch("sys.stdout", new_callable=io.StringIO):
                build_workspace.run(build.main)

            if managed_default:
                build_workspace.verify_owned(output)
                self.assertTrue((output / build_workspace.MARKER).is_file())
                self.assertFalse((output.parent / ".build-lock").exists())
            else:
                self.assertFalse((output / build_workspace.MARKER).exists())
            by_binary = {args[args.index("--bin") + 1]: args for args in compiler_calls}
            self.assertEqual(len(compiler_calls), len(build.RELEASE_CATEGORIES) + 1)
            for binary, feature in {
                "herbert": "function-compiled",
                "c04": "ubaq-compiled",
                "c06": "ordinary-bpm-compiled",
                "c08": "function-compiled",
            }.items():
                args = by_binary[binary]
                self.assertIn("--features", args)
                self.assertEqual(args[args.index("--features") + 1], feature)
            manifest = json.loads((output / "package.json").read_text(encoding="utf-8"))
            self.assertEqual(manifest["pluginCategories"], list(build.RELEASE_CATEGORIES))
            self.assertEqual(manifest["managedDecompilation"], {
                "protocolVersion": 1,
                "engineId": companion["engineId"],
                "targetRid": "win-x64",
                "sha256": companion["sha256"],
                "firstUseDownload": False,
            })
            records = {f["path"]: f for f in manifest["files"]}
            required = {
                "docs/app/technical-guide.json",
                "docs/app/user-guide.json",
                "assets/branding/creator-avatar.png",
                "assets/branding/herbert.ico",
                "herbert.exe", "RUN-DIAGNOSTICS.md", "skills/herbert/SKILL.md",
                "companions/managed-decompiler/win-x64/companion.json",
                "companions/managed-decompiler/win-x64/herbert-managed-decompiler.exe",
            }
            for family, folder, helper in (
                ("compiled-functions", "Functions", "HerbertFunctions"),
                ("compiled-bpm", "BPM directives", "HerbertBpmCompiled"),
                ("compiled-ubaq", "BAQs and UBAQs", "HerbertUbaq"),
            ):
                required.add(f"plugins/{folder}/{folder}.wasm")
                required.add(f"plugins/{folder}/plugin.json")
                for version in ("5.1.100", "5.2.100"):
                    required.add(f"plugins/{folder}/artifacts/{helper}-{version}.efxj")
                self.assertEqual(
                    (output / "plugins" / folder / "plugin.json").read_bytes(),
                    (build.ROOT / "helpers" / family / "plugin.json").read_bytes(),
                )
            notice_root = build.ROOT / "companions/managed-decompiler/notices"
            notices = json.loads((notice_root / "dependencies.json").read_text(encoding="utf-8"))
            required.update("third-party/managed-decompiler/" + f["path"] for f in notices["files"])
            required.update("third-party/managed-decompiler/" + n for n in ("dependencies.json", "ENGINE.json"))
            self.assertTrue(required <= set(records), sorted(required - set(records)))
            self.assertEqual(len(records), len(manifest["files"]))
            for relative, record in records.items():
                self.assertEqual(build.file_record(output / relative, output), record)
            self.assertEqual(
                (output / "RUN-DIAGNOSTICS.md").read_bytes(),
                (build.ROOT / "docs/RUN-DIAGNOSTICS.md").read_bytes(),
            )

    def test_managed_companion_requires_exact_rid_identity_and_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            directory = root / "win-x64"
            directory.mkdir()
            data = b"owned native executable fixture"
            name = "herbert-managed-decompiler.exe"
            (directory / name).write_bytes(data)
            manifest = dict(protocolVersion=1, engineId="ilspy-11.1.0.9782-herbert-v1", targetRid="win-x64", executable=name, bytes=len(data), sha256=hashlib.sha256(data).hexdigest(), cancelGraceMilliseconds=185)
            def save():
                (directory / "companion.json").write_text(json.dumps(manifest), encoding="utf-8")
            save()
            self.assertEqual(build.managed_companion(root, "x86_64-pc-windows-msvc")[1], manifest)
            for key, value in (("protocolVersion", 2), ("targetRid", "linux-x64"), ("executable", "../other.exe"), ("sha256", "0" * 64), ("bytes", 1)):
                previous = manifest[key]
                manifest[key] = value
                save()
                with self.assertRaises(RuntimeError):
                    build.managed_companion(root, "x86_64-pc-windows-msvc")
                manifest[key] = previous
            save()
            (directory / name).write_bytes(b"tampered")
            with self.assertRaises(RuntimeError):
                build.managed_companion(root, "x86_64-pc-windows-msvc")
            with self.assertRaises(RuntimeError):
                build.managed_companion(root, "aarch64-unknown-linux-gnu")

    def test_selected_plugin_overlay_has_one_verified_final_record(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            member = root / "plugin.json"
            member.write_bytes(b"base manifest")
            base = build.file_record(member, root)
            member.write_bytes(b"selected optional manifest")
            selected = build.file_record(member, root)
            self.assertEqual(build.final_file_records(root, [base, selected]), [selected])
            with self.assertRaises(RuntimeError):
                build.final_file_records(root, [selected, base])

    def test_approved_sources_exist(self):
        selected = build.standard_plugins(build.ROOT / "crates/herbert-plugins/src/bin")
        self.assertEqual(len(selected), len(build.RELEASE_CATEGORIES))

    def test_standard_package_contains_host_documentation(self):
        selected = build.standard_host_documents(build.ROOT)
        self.assertEqual(
            {destination.as_posix() for _, destination in selected},
            {
                "docs/app/technical-guide.json",
                "docs/app/user-guide.json",
                "AGENTS.md",
                "mcp.claude-desktop.example.json",
                "README.md",
                "skills/herbert/SKILL.md",
                "RUN-DIAGNOSTICS.md",
                "docs/Herbert-Guide.md",
                "docs/MCP-GUIDE.md",
            },
        )
        for source, _ in selected:
            self.assertTrue(source.is_file(), source)
        for source, destination in build.HOST_ASSETS:
            self.assertTrue((build.ROOT / source).is_file(), source)
        self.assertEqual({destination for _, destination in build.HOST_ASSETS}, {
            "assets/branding/herbert-vacuum.png", "assets/branding/creator-avatar.png", "assets/branding/herbert.ico",
        })

    def test_explicit_embedded_guide_paths_are_packaged(self):
        selected = {destination for _, destination in build.HOST_DOCUMENTS}
        references = set()
        for name in ("plugin_help.rs", "response_policy.rs"):
            source = build.ROOT / "crates/herbert-plugins/src" / name
            references.update("docs/" + name for name in re.findall(r"(?:docs/)?([A-Z][A-Z0-9_-]*\.md)", source.read_text(encoding="utf-8")))
        self.assertFalse(references - selected, sorted(references - selected))

    def test_missing_host_document_blocks_packaging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            for source, _ in build.HOST_DOCUMENTS:
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture", encoding="utf-8")
            (root / "docs/MCP-GUIDE.md").unlink()
            with self.assertRaisesRegex(RuntimeError, "MCP-GUIDE.md"):
                build.standard_host_documents(root)

    def test_packaged_guides_keep_their_local_link_targets(self):
        package_sources = {
            destination: source
            for source, destination in (*build.HOST_DOCUMENTS, *build.HOST_ASSETS)
        }
        package_sources["LICENSE.md"] = "LICENSE.md"
        checked = check_document_links(build.ROOT, package_sources)
        self.assertTrue(checked)


def check_document_links(root, package_sources):
    """Check every Markdown and HTML link in the selected package documents."""
    checked = []
    for document, source in package_sources.items():
        if not document.lower().endswith((".md", ".html")):
            continue
        text = (root / source).read_text(encoding="utf-8")
        text = re.sub(r"(?ms)^```.*?^```[^\n]*", "", text)
        links = re.findall(r"!?\[[^\]]*\]\(<?([^\s)>]+)>?(?:\s+[^)]*)?\)", text)
        links += re.findall(r'(?i)(?:href|src)\s*=\s*["\']([^"\']+)["\']', text)
        links += re.findall(r"(?m)^\s*\[[^\]]+\]:\s*<?([^\s>]+)", text)
        for link in links:
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc:
                continue
            target = posixpath.normpath(posixpath.join(posixpath.dirname(document), unquote(parsed.path))) if parsed.path else document
            assert target in package_sources, (document, link, target)
            target_path = root / package_sources[target]
            assert target_path.is_file(), (document, link, target)
            if parsed.fragment and target.lower().endswith(".md"):
                target_text = target_path.read_text(encoding="utf-8")
                anchors = set()
                occurrences = {}
                for heading in re.findall(r"(?m)^#{1,6}\s+(.+?)\s*#*\s*$", target_text):
                    slug = re.sub(r"[^\w\- ]", "", heading.lower()).replace(" ", "-")
                    count = occurrences.get(slug, 0)
                    occurrences[slug] = count + 1
                    anchors.add(slug + ("-" + str(count) if count else ""))
                anchors.update(re.findall(r'(?i)(?:id|name)=["\']([^"\']+)', target_text))
                assert unquote(parsed.fragment) in anchors, (document, link, "missing anchor")
            checked.append({"document": document, "link": link, "target": target})
    return checked


if __name__ == "__main__":
    unittest.main()
