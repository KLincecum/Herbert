"""Build the dedicated current custom-report reader using verified native lanes."""

import json, pathlib, uuid

root = pathlib.Path(__file__).resolve().parent
identity = root / "identity.json"
if not identity.exists():
    identity.write_text(
        json.dumps(
            {"libraryId": "HerbertReports", "globalId": str(uuid.uuid4())}, indent=2
        )
        + "\n"
    )
ident = json.loads(identity.read_text())
library_id = ident["libraryId"]
admission = (root / "src/admission.cs").read_text()


def signature(inputs, binary=False):
    out = []
    for response, parts in [
        (False, inputs),
        (
            True,
            [
                ("oSuccess", "System.Boolean"),
                ("oMessage", "System.String"),
                ("oDS", "System.Data.DataSet"),
            ]
            + ([("oContent", "System.Byte[]")] if binary else []),
        ),
    ]:
        for n, (name, kind) in enumerate(parts, 1):
            p = dict(
                Response=response,
                ParameterID=n,
                ArgumentName=name,
                Order=n,
                DataType=kind,
                Optional=False,
                Description="",
            )
            if kind == "System.Byte[]":
                p["DataTypeInfo"] = json.dumps(
                    {
                        "wellknownType": 5,
                        "namespace": "System",
                        "name": "Byte[]",
                        "options": 0,
                        "elementType": {
                            "wellknownType": 1,
                            "namespace": "System",
                            "name": "Byte",
                            "options": 1,
                        },
                    }
                )
            out.append(p)
    return out


functions = []
for name, file, inputs, binary in [
    (
        "ReadLayouts",
        "layouts.cs",
        [("iCompany", "System.String"), ("iPaths", "System.String")],
        False,
    ),
    (
        "ReadPage",
        "page.cs",
        [("iCompany", "System.String"), ("iPageSize", "System.Int32"),
         ("iHasCursor", "System.Boolean"), ("iAfter", "System.String")],
        False,
    ),
    (
        "Inventory",
        "inventory.cs",
        [
            ("iCompany", "System.String"),
            ("iPageSize", "System.Int32"),
            ("iHasCursor", "System.Boolean"),
            ("iAfter", "System.String"),
        ],
        False,
    ),
    (
        "Read",
        "read.cs",
        [("iCompany", "System.String"), ("iPrintProgram", "System.String")],
        True,
    ),
]:
    code = (
        'oSuccess=false; oMessage="NOT_STARTED"; oDS=new DataSet("HerbertReports");\n'
        + ("oContent=new Byte[0];\n" if binary else "")
    )
    code += (
        "try {\n"
        + admission
        + "\n"
        + (root / "src/scope.cs").read_text()
        + "\n"
        + (root / "src" / file).read_text()
        + '\n} catch (Exception error) { oSuccess=false; oMessage=error.Message; oDS=new DataSet("HerbertReports");'
        + ("oContent=new Byte[0];" if binary else "")
        + " }"
    )
    functions.append(
        dict(
            FunctionID=name,
            Description="Read-only report collection: " + name,
            Notes="Deployed layouts use native reads. Stored packages use CustomReportStore. No deployment or stock-store fallback.",
            Kind=2,
            RequireTransaction=False,
            SingleRowMode=False,
            Private=False,
            Disabled=False,
            Invalid=False,
            Thumbnail=None,
            FunctionSignature=signature(inputs, binary),
            Body=json.dumps(
                {
                    "Code": code,
                    "Usings": "using System;\r\nusing System.Data;\r\nusing System.Linq;",
                }
            ),
        )
    )
packages = []
for version in ["5.1.100", "5.2.100"]:
    seed = json.loads(
        (
            root.parent
            / "extended-properties"
            / ("HerbertProperties-" + version + ".efxj")
        ).read_text(encoding="utf-8-sig")
    )
    lib = dict(
        OriginalID=library_id,
        Description="Read-only deployed layouts and current custom report packages",
        GlobalID=ident["globalId"],
        EpicorVersion=version,
        Revision=1,
        Published=False,
        Private=False,
        Disabled=False,
        Mode=0,
        AllowCustomCodeWidgets=False,
        AllowCustomCodeFunctions=True,
        DirectDBAccess=1,
        Notes="Native bulk layout collection and exact CustomReportStore reads. No deployment, business writes, or generic database input.",
        OwnedByCompany="",
        Owner="",
        SharedWithGroup="",
        Functions=functions,
        LibraryReferences=[
            {
                "ReferenceType": 1,
                "ReferenceID": "ICE.CustomReportStore",
                "Updatable": False,
            },
            {"ReferenceType": 2, "ReferenceID": "ICE:BO:UserFile"},
            {"ReferenceType": 0, "ReferenceID": "Ice.Contracts.BO.UserFile.dll"},
            {"ReferenceType": 2, "ReferenceID": "ICE:BO:Report"},
            {"ReferenceType": 0, "ReferenceID": "Ice.Contracts.BO.Report.dll"},
            {"ReferenceType": 0, "ReferenceID": "Newtonsoft.Json.dll"},
        ],
        LibraryMappings=[],
    )
    package = {k: seed[k] for k in ["Mode", "Version", "SystemCode"]}
    package.update(LibraryId=library_id, Library=lib)
    lib["LibraryReferences"].extend(
        [
            {"ReferenceType": 0, "ReferenceID": "Ice.Core.TaskBase.dll"},
            {"ReferenceType": 0, "ReferenceID": "Ice.Lib.RoutingBreakingRules.dll"},
        ]
    )
    (root / (library_id + "-" + version + ".efxj")).write_text(
        json.dumps(package, indent=2) + "\n", encoding="utf-8-sig"
    )
    packages.append({"epicorVersion": version, "package": package})
contract = dict(
    libraryId=library_id,
    globalId=ident["globalId"],
    companyArgument="iCompany",
    functions=[
        dict(functionId="ReadLayouts", binaryOutputs=[], effect="read"),
        dict(functionId="ReadPage", binaryOutputs=[], effect="read"),
        dict(functionId="Inventory", binaryOutputs=[], effect="read"),
        dict(functionId="Read", binaryOutputs=["oContent"], effect="read"),
    ],
    installation=dict(supportsInstances=True, packages=packages),
)
(root / "helper-contract.json").write_text(json.dumps(contract, indent=2) + "\n")
print(
    json.dumps(
        {
            "library": library_id,
            "versions": [p["epicorVersion"] for p in packages],
            "functions": len(functions),
        }
    )
)
