"""Build version-specific packages from the single BAQ export Function source."""
from pathlib import Path
import json

ROOT = Path(__file__).resolve().parent
for version in ("5.1.100", "5.2.100"):
    package = json.loads((ROOT.parent / "host" / f"HerbertHost-{version}.efxj").read_text(encoding="utf-8-sig"))
    package["LibraryId"] = "HerbertBaqExport"
    library = package["Library"]
    library.update(OriginalID="HerbertBaqExport", Description="Export selected BAQ batches into host-owned staging.",
                   GlobalID="17d52db7-ae06-4a79-96b6-98c2ad663b47")
    function = library["Functions"][0]
    function.update(FunctionID="ExportBatch", Description="Export selected non-updatable BAQs without executing them.")
    function["FunctionSignature"] = [
        {"Response": response, "ParameterID": order, "ArgumentName": name, "Order": order,
         "DataType": data_type, "Optional": False, "Description": ""}
        for response, order, name, data_type in [
            (False, 1, "iRequest", "System.String"),
            (True, 1, "oSuccess", "System.Boolean"),
            (True, 2, "oMessage", "System.String"),
            (True, 3, "oJson", "System.String"),
        ]
    ]
    function["Body"] = json.dumps({"Code": (ROOT / "src/export-batch.cs").read_text(encoding="utf-8"), "Usings": ""})
    library["LibraryReferences"] += [
        {"ReferenceType": 0, "ReferenceID": "System.Xml.Linq.dll"},
        {"ReferenceType": 0, "ReferenceID": "System.Net.Http.dll"},
    ]
    (ROOT / f"HerbertBaqExport-{version}.efxj").write_text(json.dumps(package, separators=(",", ":")), encoding="utf-8-sig")
    print(version)
