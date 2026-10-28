"""Build the host utility packages. Native validation remains mandatory."""

from pathlib import Path
import json

ROOT = Path(__file__).resolve().parent
code = (ROOT / "src/execute.cs").read_text(encoding="utf-8")
lease_code = (ROOT / "src/lease.cs").read_text(encoding="utf-8")
for version in ["4.3.200", "5.1.100", "5.2.100"]:
    # ICE stamps come from native exports, not application/database versions.
    # 2024.2 rejects 5.1.100 in CheckSourceVersion; the unchanged bodies were
    # imported, compiled and exercised in a native 4.3.200 package.
    library = {
        "OriginalID": "HerbertHost",
        "Description": "Run-owned Herbert server packaging and transfer utility.",
        "Notes": "Herbert lease host v1",
        "GlobalID": "a0b40769-4b6a-4ef0-a755-edb8c446b9a4",
        "EpicorVersion": version,
        "Revision": 1,
        "Published": False,
        "Private": False,
        "Disabled": False,
        "Mode": 0,
        "AllowCustomCodeWidgets": False,
        "AllowCustomCodeFunctions": True,
        "DirectDBAccess": 0,
        "OwnedByCompany": "",
        "Owner": "",
        "SharedWithGroup": "",
        "Functions": [
            {
                "FunctionID": "Execute",
                "Description": "Operate on an exact run-owned staging result.",
                "Kind": 2,
                "RequireTransaction": False,
                "SingleRowMode": False,
                "Private": False,
                "Disabled": False,
                "Invalid": False,
                "Thumbnail": None,
                "FunctionSignature": [
                    {
                        "Response": False,
                        "ParameterID": 1,
                        "ArgumentName": "iRequest",
                        "Order": 1,
                        "DataType": "System.String",
                        "Optional": False,
                        "Description": "",
                    },
                    {
                        "Response": True,
                        "ParameterID": 1,
                        "ArgumentName": "oJson",
                        "Order": 1,
                        "DataType": "System.String",
                        "Optional": False,
                        "Description": "",
                    },
                ],
                # Epicor supplies System, LINQ and generic collections implicitly.
                # All other types in this body use their full namespace.
                "Body": json.dumps({"Code": code, "Usings": ""}),
            }
        ],
        "LibraryReferences": [
            {"ReferenceType": 0, "ReferenceID": "System.IO.Compression.dll"},
            {"ReferenceType": 0, "ReferenceID": "Newtonsoft.Json.dll"},
        ],
        "LibraryMappings": [],
    }
    lease = json.loads(json.dumps(library["Functions"][0]))
    lease["FunctionID"] = "Lease"
    lease["Description"] = "Renew activity and claim expired temporary helpers."
    lease["Body"] = json.dumps({"Code": lease_code, "Usings": ""})
    library["Functions"].append(lease)
    package = {
        "Mode": "Backup",
        "Version": version,
        "SystemCode": "ICE",
        "LibraryId": "HerbertHost",
        "Library": library,
    }
    (ROOT / f"HerbertHost-{version}.efxj").write_text(
        json.dumps(package, separators=(",", ":")), encoding="utf-8-sig"
    )
    print(version, library["EpicorVersion"])
