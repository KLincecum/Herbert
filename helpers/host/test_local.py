"""Compile/run the exact helper body against an explicitly offline Sandbox shim."""

from pathlib import Path
import json, os, subprocess, tempfile, sys, textwrap

ROOT = Path(__file__).resolve().parent
manifest = json.loads(Path("C:/Tools/Workbench/manifests/installed.json").read_text())


def find_dotnet(value):
    if isinstance(value, dict):
        for child in value.values():
            found = find_dotnet(child)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = find_dotnet(child)
            if found:
                return found
    elif (
        isinstance(value, str)
        and value.lower().endswith("dotnet.exe")
        and Path(value).is_file()
    ):
        return Path(value)


dotnet = find_dotnet(manifest)
if not dotnet:
    raise RuntimeError("No manifest-owned .NET SDK")
newtonsoft = next((dotnet.parent / "sdk").glob("*/Newtonsoft.Json.dll"))
build = Path("C:/Tools/Workbench/workspace/build/hoover/host-helper-tests")
build.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="run-", dir=build) as temp:
    work = Path(temp)
    (
        work / "Test.csproj"
    ).write_text(f"""<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework><ImplicitUsings>disable</ImplicitUsings>
<Nullable>disable</Nullable><TreatWarningsAsErrors>true</TreatWarningsAsErrors></PropertyGroup>
<ItemGroup><Reference Include="Newtonsoft.Json"><HintPath>{newtonsoft}</HintPath></Reference></ItemGroup></Project>""")
    (work / "Harness.cs").write_text(
        (ROOT / "tests/Harness.cs").read_text(encoding="utf-8"), encoding="utf-8"
    )
    (work / "Body.cs").write_text(
        "using System;\nusing System.Linq;\nusing System.Collections.Generic;\nusing DateTime = LeaseClock;\npublic partial class Context { private void Run() {\n// BEGIN FUNCTION BODY\n"
        + (ROOT / "src/execute.cs").read_text(encoding="utf-8")
        + "\n// END FUNCTION BODY\n}}\n",
        encoding="utf-8",
    )
    env = os.environ.copy()
    (work / "Lease.cs").write_text(
        "using System;\nusing System.Linq;\nusing System.Collections.Generic;\nusing DateTime = LeaseClock;\npublic partial class Context { private void RunLease() {\n// BEGIN LEASE BODY\n"
        + (ROOT / "src/lease.cs").read_text(encoding="utf-8")
        + "\n// END LEASE BODY\n}}\n",
        encoding="utf-8",
    )
    (work / "LeaseChecks.cs").write_text(
        (ROOT / "tests/LeaseChecks.cs").read_text(encoding="utf-8"), encoding="utf-8"
    )
    env.setdefault("ProgramFiles", r"C:\Program Files")
    env.setdefault("ProgramFiles(x86)", r"C:\Program Files (x86)")
    env.setdefault("APPDATA", str(Path.home() / "AppData/Roaming"))
    env["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1"
    subprocess.run(
        [
            str(dotnet),
            "run",
            "--project",
            str(work / "Test.csproj"),
            "--configuration",
            "Release",
        ],
        env=env,
        check=True,
    )
    if "--format" in sys.argv:
        (work / ".editorconfig").write_text("""root = true
[*.cs]
indent_style = space
indent_size = 4
csharp_new_line_before_open_brace = all
csharp_preserve_single_line_blocks = false
csharp_preserve_single_line_statements = false
""")
        subprocess.run(
            [
                str(dotnet),
                "format",
                "whitespace",
                str(work / "Test.csproj"),
                "--no-restore",
            ],
            env=env,
            check=True,
        )
        body = (work / "Body.cs").read_text(encoding="utf-8-sig")
        body = body.split("// BEGIN FUNCTION BODY\n", 1)[1].split(
            "// END FUNCTION BODY", 1
        )[0]
        (ROOT / "src/execute.cs").write_text(
            textwrap.dedent(body).rstrip() + "\n", encoding="utf-8"
        )
        (ROOT / "tests/Harness.cs").write_text(
            (work / "Harness.cs").read_text(encoding="utf-8-sig"), encoding="utf-8"
        )
        lease = (work / "Lease.cs").read_text(encoding="utf-8-sig")
        lease = lease.split("// BEGIN LEASE BODY\n", 1)[1].split(
            "// END LEASE BODY", 1
        )[0]
        (ROOT / "src/lease.cs").write_text(
            textwrap.dedent(lease).rstrip() + "\n", encoding="utf-8"
        )
        (ROOT / "tests/LeaseChecks.cs").write_text(
            (work / "LeaseChecks.cs").read_text(encoding="utf-8-sig"), encoding="utf-8"
        )
