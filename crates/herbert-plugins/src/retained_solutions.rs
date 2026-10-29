//! Retained installed packages through the native read-only ExportPackage contract.
//! Source: ExportPackageSvc.GetInstalledSolutions (3868-3887) and GetInstalledSolution
//! (3774-3784), ICE 5.2.100.9. SolutionInstall is not accessible through DynamicQuery.
//! Do not replace this route with BuildSolution: the native build mutates source state.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
const KIND: &str = "RetainedInstalledSolution";
const SERVICE: &str = "Ice.BO.ExportPackageSvc";
const METADATA_FIELDS: [&str; 8] = [
    "CompanyID",
    "SolutionID",
    "Description",
    "FileName",
    "BuildIteration",
    "SolutionReference",
    "ChangedBy",
    "ChangedOn",
];
pub fn owns(category: &str, kind: &str) -> bool {
    category == "solutions-and-packages" && kind == KIND
}
pub fn methods() -> Vec<(String, String)> {
    ["GetInstalledSolutions", "GetInstalledSolution"]
        .iter()
        .map(|m| (SERVICE.into(), (*m).into()))
        .collect()
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn read(method: &str, arguments: Value, export: bool, state: Value) -> GuestReply {
    GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: SERVICE.into(),
            method: method.into(),
            arguments,
            export,
        },
        state,
    }
}
fn identity(row: &Value, company: &str) -> Result<Identity, Diagnostic> {
    let company_id = row["CompanyID"].as_str().ok_or_else(|| {
        Diagnostic::new(
            "RETAINED_SOLUTION_IDENTITY_UNVERIFIED",
            "The installed package catalog omitted CompanyID.",
        )
    })?;
    let solution_id = row["SolutionID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Diagnostic::new(
                "RETAINED_SOLUTION_IDENTITY_UNVERIFIED",
                "The installed package catalog omitted SolutionID.",
            )
        })?;
    if company_id != company {
        return Err(Diagnostic::new(
            "SCOPE_MISMATCH",
            "The installed package catalog returned another company.",
        ));
    }
    Ok(Identity {
        category: "solutions-and-packages".into(),
        kind: KIND.into(),
        scope: Scope::Company {
            company: company.into(),
        },
        keys: BTreeMap::from([
            ("Company".into(), company.into()),
            ("SolutionID".into(), solution_id.into()),
        ]),
    })
}
fn metadata(row: &Value) -> Result<Value, Diagnostic> {
    for field in METADATA_FIELDS {
        if row.get(field).is_none() {
            return Err(Diagnostic::new("RETAINED_SOLUTION_METADATA_UNVERIFIED",
                "The installed package catalog omitted Epicor metadata fields."));
        }
    }
    // Preserve the complete native catalog row, including history and future fields.
    Ok(row.clone())
}
pub(super) fn catalog(response: &Value, company: &str) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let rows = response["returnObj"].as_array().ok_or_else(|| {
        Diagnostic::new(
            "RETAINED_SOLUTION_INVENTORY_UNVERIFIED",
            "The Epicor installed-package catalog is not an array.",
        )
    })?;
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if crate::system_artifacts::marked(row) {
            continue;
        }
        let id = identity(row, company)?;
        if !seen.insert(id.keys["SolutionID"].clone()) {
            return Err(Diagnostic::new(
                "DUPLICATE_NATIVE_IDENTITY",
                "The Epicor installed-package catalog repeats a company and solution identity.",
            ));
        }
        output.push((id, metadata(row)?));
    }
    Ok(output)
}
fn discovery(request: GuestRequest) -> GuestReply {
    if request.cursor.is_some() {
        return fail(
            "INVALID_CURSOR",
            "The Epicor installed-package catalog is unpaged and has no resume point saved position.",
        );
    }
    if request.response.is_none() && request.error.is_none() {
        return read("GetInstalledSolutions", json!({}), false, Value::Null);
    }
    if let Some(error) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: None,
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    match catalog(request.response.as_ref().unwrap(), &request.company) {
        Ok(rows) => GuestReply::Inventory {
            identities: rows.into_iter().map(|(id, _)| id).collect(),
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        },
        Err(error) => GuestReply::Inventory {
            identities: vec![],
            next_cursor: None,
            authoritative: false,
            diagnostics: vec![error],
        },
    }
}
pub(super) fn inline_bytes(payload: &Value) -> Result<Vec<u8>, Diagnostic> {
    let invalid = || Diagnostic::new("RETAINED_SOLUTION_BYTES_UNVERIFIED",
        "The Epicor retained package has an invalid byte payload.");
    match payload {
        Value::String(s) => base64::engine::general_purpose::STANDARD.decode(s).map_err(|_| invalid()),
        Value::Array(a) => a.iter().map(|v| v.as_u64().filter(|n| *n <= 255)
            .map(|n| n as u8).ok_or_else(invalid)).collect(),
        _ => Err(invalid()),
    }
}
pub(super) fn package_signature(response: &Value) -> Result<Value, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "RETAINED_SOLUTION_BYTES_UNVERIFIED",
            "The Epicor retained package lacks a physical row ID or valid byte payload.",
        )
    };
    let id = response["returnObj"].as_str().ok_or_else(invalid)?;
    if id.eq_ignore_ascii_case("00000000-0000-0000-0000-000000000000") {
        return Err(Diagnostic::new(
            "RETAINED_SOLUTION_DISAPPEARED",
            "The Epicor retained package returned its explicit absent sentinel.",
        ));
    }
    if id.len() != 36
        || id.bytes().enumerate().any(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b != b'-'
            } else {
                !b.is_ascii_hexdigit()
            }
        })
    {
        return Err(invalid());
    }
    let payload = response
        .pointer("/parameters/content")
        .ok_or_else(invalid)?;
    if payload.is_null() {
        return Err(Diagnostic::new(
            "RETAINED_SOLUTION_CONTENT_UNAVAILABLE",
            "The installed solution row has no retained package bytes.",
        ));
    }
    let (bytes, hash) = if let Some(file) = payload.get("nativeFile") {
        let file: FileRef = serde_json::from_value(file.clone()).map_err(|_| invalid())?;
        if file.path.is_empty()
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        (file.bytes, file.sha256.to_ascii_lowercase())
    } else {
        let bytes = inline_bytes(payload)?;
        (bytes.len() as u64, format!("{:x}", Sha256::digest(&bytes)))
    };
    if bytes == 0 {
        return Err(Diagnostic::new(
            "RETAINED_SOLUTION_CONTENT_UNAVAILABLE",
            "The installed solution row has no retained package bytes.",
        ));
    }
    Ok(json!({"SysRowID":id.to_ascii_lowercase(),"bytes":bytes,"sha256":hash}))
}
pub(super) fn declare_package_output(data: &mut Value) {
    data["herbertDirectOutput"] = json!(true);
    // Only a validated native package can be declared as a recovery artifact.
    if data["packageSignature"].is_null() { return; }
        let leaf = data["metadata"]["FileName"]
            .as_str()
            .unwrap_or("retained-package.zip")
            .rsplit(['/', '\\'])
            .next()
            .unwrap();
        let name = format!(
            "package-{}",
            leaf.chars()
                .map(|c| if c.is_alphanumeric() || "._- ".contains(c) {
                    c
                } else {
                    '_'
                })
                .collect::<String>()
        );
        if let Some(file) = data
            .pointer("/package/parameters/content/nativeFile")
            .cloned()
        {
            data["herbertSources"]["nativeFiles"] = json!([{"name":name,"file":file}]);
        } else if let Some(payload) = data.pointer("/package/parameters/content") {
            if let Ok(bytes) = crate::retained_solutions::inline_bytes(payload) {
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                data["herbertSources"]["binaryFiles"] = json!([{"name":name,"base64":encoded}]);
            }
        }
}
fn finish(request: GuestRequest) -> GuestReply {
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(request.state["errors"].clone()).unwrap_or_default();
    let mut definition = json!({"metadata":request.state["metadata"],"package":request.state["package"],"packageSignature":request.state["packageSignature"]});
    declare_package_output(&mut definition);
    GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "retained-installed-solution-package-json".into(),
        definition,
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    }
}
fn record_error(request: &mut GuestRequest, error: Diagnostic) {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(error));
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation == "discover" {
        return discovery(request);
    }
    if request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The retained-package collector supports discovery and exact download.",
        );
    }
    let Some(id) = request.identity.as_ref() else {
        return fail(
            "IDENTITY_REQUIRED",
            "Select an exact retained-package identity.",
        );
    };
    if !owns(&request.category, &id.kind)
        || id.category != request.category
        || identity(
            &json!({"CompanyID":id.keys.get("Company"),"SolutionID":id.keys.get("SolutionID")}),
            &request.company,
        )
        .ok()
        .as_ref()
            != Some(id)
    {
        return fail(
            "INVALID_IDENTITY",
            "Use the exact retained-package identity from discovery.",
        );
    }
    let phase = request.state["phase"]
        .as_str()
        .unwrap_or("initial")
        .to_string();
    if phase == "initial" {
        return read(
            "GetInstalledSolutions",
            json!({}),
            false,
            json!({"phase":"metadata","errors":[]}),
        );
    }
    if let Some(error) = request.error.take() {
        if phase != "metadata" {
            record_error(&mut request, error);
            return finish(request);
        }
        return GuestReply::Failure { diagnostic: error };
    }
    let Some(response) = request.response.as_ref() else {
        return fail(
            "RETAINED_SOLUTION_RESPONSE_MISSING",
            "The retained-package response is absent.",
        );
    };
    if phase == "metadata" {
        let found = catalog(response,&request.company).and_then(|rows| rows.into_iter().find(|(found,_)|found==id).map(|(_,meta)|meta).ok_or_else(||Diagnostic::new("RETAINED_SOLUTION_DISAPPEARED", "The selected solution is absent from the Epicor installed-package catalog.")));
        let meta = match found {
            Ok(meta) => meta,
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        };
        return read(
            "GetInstalledSolution",
            json!({"solutionID":id.keys["SolutionID"]}),
            true,
            json!({"phase":"package","metadata":meta,"errors":[]}),
        );
    }
    if phase == "package" {
        request.state["package"] = response.clone();
        match package_signature(response) {
            Ok(signature) => request.state["packageSignature"] = signature,
            Err(error) => record_error(&mut request, error),
        }
        return finish(request);
    }
    fail(
        "INVALID_PLUGIN_STATE",
        "The retained-package saved progress is invalid.",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Value {
        json!({"CompanyID":"C","SolutionID":"S","Description":"Source","FileName":"S.zip_AllCompany","BuildIteration":1,"SolutionReference":"R","ChangedBy":"u","ChangedOn":"2026-10-18T00:00:00","InstallLog":"execution log"})
    }
    fn package() -> Value {
        json!({"returnObj":"00000000-0000-0000-0000-000000000001","parameters":{"content":"UEs="}})
    }
    fn request(phase: &str, response: Value) -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "solutions-and-packages".into(),
            company: "C".into(),
            identity: Some(identity(&row(), "C").unwrap()),
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":phase,"metadata":metadata(&row()).unwrap(),"package":package(),"packageSignature":package_signature(&package()).unwrap(),"errors":[]}),
            response: Some(response),
            error: None,
        }
    }
    #[test]
    fn catalog_keeps_exact_filename_and_uses_native_primary_key() {
        let records = catalog(&json!({"returnObj":[row()]}), "C").unwrap();
        assert_eq!(records[0].0.keys.len(), 2);
        assert_eq!(records[0].1["FileName"], "S.zip_AllCompany");
        assert_eq!(records[0].1["InstallLog"], "execution log");
        assert!(catalog(&json!({"returnObj":[row(),row()]}), "C").is_err());
        assert!(catalog(&json!({"returnObj":[row()]}), "other").is_err());
    }
    #[test]
    fn package_signature_checks_physical_row_and_bytes() {
        let signature = package_signature(&package()).unwrap();
        assert_eq!(signature["bytes"], 2);
        assert_eq!(signature["sha256"], format!("{:x}", Sha256::digest(b"PK")));
        let absent =
            json!({"returnObj":"00000000-0000-0000-0000-000000000000","parameters":{"content":""}});
        assert_eq!(
            package_signature(&absent).unwrap_err().code,
            "RETAINED_SOLUTION_DISAPPEARED"
        );
        let mut empty = package();
        empty["parameters"]["content"] = json!("");
        assert_eq!(
            package_signature(&empty).unwrap_err().code,
            "RETAINED_SOLUTION_CONTENT_UNAVAILABLE"
        );
    }
    #[test]
    fn downloaded_package_is_completed_without_confirmation_reads() {
        let GuestReply::Definition {
            definition,
            completeness,
            ..
        } = handle(request("package", package()))
        else {
            panic!("The downloaded package needs no second server read.")
        };
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(definition["package"]["parameters"]["content"], "UEs=");
    }

    #[test]
    fn residual0033_exact_declares_original_inline_package_and_preserves_metadata() {
        let bytes = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        for payload in [json!(base64::engine::general_purpose::STANDARD.encode(bytes)), json!(bytes.to_vec())] {
            let mut response = package();
            response["parameters"]["content"] = payload;
            let original = response.clone();
            let mut r = request("package", response);
            r.state["metadata"]["FutureField"] = json!({"retained":true});
            r.state["metadata"]["FileName"] = json!("folder\\S.zip");
            let expected_metadata = r.state["metadata"].clone();
            let GuestReply::Definition { definition, completeness, diagnostics, .. } = handle(r) else { panic!("definition expected") };
            assert_eq!(completeness, Completeness::Collected);
            assert!(diagnostics.is_empty());
            assert_eq!(definition["metadata"], expected_metadata);
            assert_eq!(definition["metadata"]["InstallLog"], "execution log");
            assert_eq!(definition["package"], original);
            assert_eq!(definition["herbertDirectOutput"], true);
            let output = &definition["herbertSources"]["binaryFiles"][0];
            assert_eq!(output["name"], "package-S.zip");
            assert_eq!(inline_bytes(&output["base64"]).unwrap(), bytes);
            assert_eq!(definition["packageSignature"]["sha256"], format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(definition["packageSignature"]["bytes"], bytes.len());
            assert!(definition["herbertSources"].get("nativeFiles").is_none());
        }
    }
    #[test]
    fn residual0033_exact_declares_retained_file_without_rewriting_it() {
        let mut response = package();
        let file = json!({"path":"Herbert Data/raw/original.bin","sha256":"a".repeat(64),"bytes":22,"role":"native"});
        response["parameters"]["content"] = json!({"nativeFile":file});
        let original = response.clone();
        let GuestReply::Definition { definition, completeness, .. } = handle(request("package", response)) else { panic!("definition expected") };
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(definition["package"], original);
        assert_eq!(definition["herbertSources"]["nativeFiles"], json!([{"name":"package-S.zip_AllCompany","file":file}]));
        assert!(definition["herbertSources"].get("binaryFiles").is_none());
        assert_eq!(definition["metadata"]["InstallLog"], "execution log");
    }
    #[test]
    fn residual0033_invalid_or_unavailable_package_keeps_metadata_without_output() {
        for payload in [Value::Null, json!([256]), json!(""), json!("not base64!")] {
            let mut response = package();
            response["parameters"]["content"] = payload;
            let mut r = request("package", response);
            r.state["packageSignature"] = Value::Null;
            let GuestReply::Definition { definition, completeness, diagnostics, .. } = handle(r) else { panic!("definition expected") };
            assert_eq!(completeness, Completeness::Partial);
            assert!(!diagnostics.is_empty());
            assert_eq!(definition["metadata"]["InstallLog"], "execution log");
            assert!(definition.get("herbertSources").is_none());
        }
    }

}
