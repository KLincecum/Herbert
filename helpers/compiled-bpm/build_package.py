"""Regenerate ordinary BPM readers while preserving established package formatting.

The packager copies these helper assets into plugins/BPM directives/artifacts.
No separate packages/bpm-directives source mirror or production host embedding is used.
"""
from pathlib import Path
import json

ROOT = Path(__file__).resolve().parent
VERSIONS = ("4.3.200", "5.1.100", "5.2.100")

def build():
    code = (ROOT / "src/read-compiled.cs").read_text(encoding="utf-8")
    for version in VERSIONS:
        destination = ROOT / f"HerbertBpmCompiled-{version}.efxj"
        package = json.loads(destination.read_text(encoding="utf-8-sig"))
        if package["Version"] != version or package["Library"]["EpicorVersion"] != version:
            raise ValueError("Preserve the target-native release stamp in both package fields.")
        functions = package["Library"]["Functions"]
        if len(functions) != 1 or functions[0]["FunctionID"] != "ReadCompiled":
            raise ValueError("The shipping ordinary BPM reader must contain only ReadCompiled.")
        functions[0]["Body"] = json.dumps({"Code": code, "Usings": ""}, separators=(",", ":"), ensure_ascii=False)
        # 4.3 retains its exact compact native-tested bytes. The later
        # packages retain their established indentation and no final newline.
        options = {"separators": (",", ":")} if version == "4.3.200" else {"indent": 2}
        payload = json.dumps(package, ensure_ascii=False, **options)
        destination.write_bytes(b"\xef\xbb\xbf" + payload.encode("utf-8"))
        print(f"{destination.name}: one read-only ReadCompiled Function")

if __name__ == "__main__":
    build()
