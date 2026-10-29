"""Update versioned native packages from the single Extended Properties reader."""

from pathlib import Path
import json

ROOT = Path(__file__).resolve().parent
PLUGIN = ROOT.parents[1] / "crates/herbert-plugins/packages/extended-properties/artifacts"


def build() -> None:
    code = (ROOT / "src/read-all-properties.cs").read_text(encoding="utf-8")
    for version in ("4.3.200", "5.1.100", "5.2.100"):
        name = f"HerbertProperties-{version}.efxj"
        package = json.loads((ROOT / name).read_text(encoding="utf-8-sig"))
        if package["Version"] != version or package["Library"]["EpicorVersion"] != version:
            raise ValueError("The package must retain its target-native release stamp.")
        functions = package["Library"]["Functions"]
        if len(functions) != 1 or functions[0]["FunctionID"] != "ReadAllProperties":
            raise ValueError("Extended properties must contain one ReadAllProperties Function.")
        functions[0]["Body"] = json.dumps({"Code": code, "Usings": ""})
        # Preserve the exact tested 4.3 payload and existing later-package bytes.
        payload = (json.dumps(package, separators=(",", ":")) if version == "4.3.200"
                   else json.dumps(package, indent=2) + "\n")
        for destination in (ROOT / name, PLUGIN / name):
            destination.write_text(payload, encoding="utf-8-sig")
        print(f"{name}: one Function; source and plugin packages match")


if __name__ == "__main__":
    build()
