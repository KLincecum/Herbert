//! Authored MetaFX applications, individual layers, drafts and retained versions.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn methods() -> Vec<(String, String)> {
    ["GetApplications", "GetLayer", "ExportApp"]
        .into_iter()
        .map(|m| ("Ice.Lib.MetaFXSvc".into(), m.into()))
        .chain(std::iter::once((
            "Ice.Lib.UxAppVersionSvc".into(),
            "GetUxAppVersions".into(),
        )))
        .collect()
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn action(service: &str, method: &str, args: Value, export: bool, state: Value) -> GuestReply {
    GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: service.into(),
            method: method.into(),
            arguments: args,
            export,
        },
        state,
    }
}
fn object_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
fn body(value: &Value) -> &Value {
    value.get("returnObj").unwrap_or(value)
}
fn personal(code: &str) -> bool {
    code.eq_ignore_ascii_case("KNTCPersLayer") || code.eq_ignore_ascii_case("KineticGridLayout")
}
fn layer_identity(
    category: &str,
    layer: &Value,
    view: &str,
    company: &str,
) -> Result<Identity, Diagnostic> {
    let get = |field: &str| object_text(layer, field).unwrap_or("");
    let actual_company = get("Company");
    if !actual_company.is_empty() && actual_company != company {
        return Err(Diagnostic::new(
            "SCOPE_MISMATCH",
            "A layer belongs to a company outside this run.",
        ));
    }
    if get("TypeCode").is_empty() || get("DeviceType").is_empty() {
        return Err(Diagnostic::new(
            "LAYER_IDENTITY_UNVERIFIED",
            "A layer lacks its native type or device identity.",
        ));
    }
    let owner = get("UserName");
    let scope = if personal(get("TypeCode")) && !owner.is_empty() {
        Scope::Personal {
            company: actual_company.into(),
            owner: owner.into(),
        }
    } else if actual_company.is_empty() {
        Scope::Global
    } else {
        Scope::Company {
            company: company.into(),
        }
    };
    let keys = BTreeMap::from([
        (
            "ViewId".into(),
            if get("ViewId").is_empty() {
                view
            } else {
                get("ViewId")
            }
            .into(),
        ),
        ("Company".into(), actual_company.into()),
        ("TypeCode".into(), get("TypeCode").into()),
        ("LayerName".into(), get("LayerName").into()),
        ("DeviceType".into(), get("DeviceType").into()),
        ("CGCCode".into(), get("CGCCode").into()),
        ("UserName".into(), owner.into()),
    ]);
    Ok(Identity {
        category: category.into(),
        kind: "MetaFXLayer".into(),
        scope,
        keys,
    })
}
fn discover(request: GuestRequest) -> GuestReply {
    // Application summary projections omit personal owner/parent identity. C13
    // uses the independent paged XXXDef families declared in tables.json.
    if request.category == "C13" {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        };
    }
    let published = match request.cursor.as_deref() {
        None => false,
        Some("published") => true,
        _ => return fail("INVALID_CURSOR", "The MetaFX discovery cursor is invalid."),
    };
    if request.response.is_none() && request.error.is_none() {
        return action(
            "Ice.Lib.MetaFXSvc",
            "GetApplications",
            // The native contract declares Type nullable, but 12.2.100.9 fails
            // inside GetApplications for null. Empty Type means all app types.
            // Single-field native probes verified both publication branches.
            json!({"request":{"Type":"","SubType":null,"SearchText":null,"IsPublished":published,"IncludeAllLayers":true,"IncludePersLayers":false}}),
            false,
            json!({"phase":"discover"}),
        );
    }
    // IsPublished=false includes published and unpublished records in the native
    // GetLayersPaged implementation. Retain legacy published cursor readability.
    let next_cursor = None;
    if let Some(error) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor,
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    let Some(apps) = request
        .response
        .as_ref()
        .map(body)
        .and_then(Value::as_array)
    else {
        return fail(
            "INVALID_NATIVE_RESPONSE",
            "MetaFX application discovery did not return its application array.",
        );
    };
    let mut identities = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = BTreeSet::new();
    for app in apps {
        let Some(view) = object_text(app, "Id").filter(|s| !s.is_empty()) else {
            diagnostics.push(Diagnostic::new(
                "APP_IDENTITY_UNVERIFIED",
                "A native application lacks its exact Id.",
            ));
            continue;
        };
        if request.category == "C12" && !crate::system_artifacts::marked(app) {
            identities.push(Identity {
                category: "C12".into(),
                kind: "MetaFXApplication".into(),
                scope: Scope::Company {
                    company: request.company.clone(),
                },
                keys: BTreeMap::from([
                    ("ViewId".into(), view.into()),
                    ("Company".into(), request.company.clone()),
                    ("Type".into(), object_text(app, "Type").unwrap_or("").into()),
                    (
                        "SubType".into(),
                        object_text(app, "SubType").unwrap_or("").into(),
                    ),
                    ("DeviceType".into(), "Desktop".into()),
                ]),
            });
        }
        if let Some(layers) = app.get("Layers").and_then(Value::as_array) {
            for layer in layers {
                if crate::system_artifacts::marked(layer) {
                    continue;
                }
                let code = object_text(layer, "TypeCode").unwrap_or("");
                if personal(code) != (request.category == "C13") {
                    continue;
                }
                match layer_identity(&request.category, layer, view, &request.company) {
                    Ok(id) => identities.push(id),
                    Err(e) => diagnostics.push(e),
                }
            }
        } // A missing Layers value is the native representation of no matching rows.
    }
    identities.retain(|id| seen.insert(serde_json::to_string(id).unwrap()));
    GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: diagnostics.is_empty(),
        diagnostics,
    }
}
fn layer_request(id: &Identity, caller: &str, draft: bool, version: i64) -> Value {
    json!({"request":{"ViewId":id.keys["ViewId"],"Company":id.keys["Company"],"TypeCode":id.keys["TypeCode"],
        "LayerName":id.keys["LayerName"],"DeviceType":id.keys["DeviceType"],"CGCCode":id.keys["CGCCode"],
        "UserName":if id.keys["UserName"].is_empty(){caller}else{&id.keys["UserName"]},"ParentLayers":null,"PageName":null,
        "IncludeDraftContent":draft,"UxAppVersion":version}})
}
pub fn owns(category: &str, kind: &str) -> bool {
    matches!(category, "C12" | "C13") && matches!(kind, "MetaFXApplication" | "MetaFXLayer")
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation == "discover" {
        return discover(request);
    }
    if request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The MetaFX collector supports discovery and exact-identity reads.",
        );
    }
    let Some(id) = request.identity.as_ref() else {
        return fail("IDENTITY_REQUIRED", "Select an exact MetaFX identity.");
    };
    let layer = id.kind == "MetaFXLayer";
    let expected: &[&str] = if layer {
        &[
            "ViewId",
            "Company",
            "TypeCode",
            "LayerName",
            "DeviceType",
            "CGCCode",
            "UserName",
        ]
    } else {
        &["ViewId", "Company", "Type", "SubType", "DeviceType"]
    };
    if id.category != request.category
        || !owns(&request.category, &id.kind)
        || id.keys.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != expected.iter().copied().collect::<BTreeSet<_>>()
    {
        return fail(
            "INVALID_IDENTITY",
            "The complete native MetaFX identity is required.",
        );
    }
    if !id.keys["Company"].is_empty() && id.keys["Company"] != request.company {
        return fail(
            "SCOPE_MISMATCH",
            "The MetaFX identity belongs to another company.",
        );
    }
    let caller = request
        .state
        .get("_hostCaller")
        .and_then(Value::as_str)
        .unwrap_or("");
    if caller.is_empty() {
        return fail(
            "CALLER_REQUIRED",
            "The host must supply the authenticated caller identity for the native read contract.",
        );
    }
    if layer && personal(&id.keys["TypeCode"]) && id.keys["UserName"].is_empty() {
        return fail(
            "PERSONAL_OWNER_UNVERIFIED",
            "The personal layer inventory does not establish its native owner. Do not substitute the editor or initiating caller for the owner.",
        );
    }
    let expected_scope = if layer && personal(&id.keys["TypeCode"]) {
        if request.category != "C13" {
            return fail(
                "INVALID_IDENTITY",
                "Personal Kinetic definitions belong to C13.",
            );
        }
        if id.keys["UserName"] != caller {
            return fail(
                "PERSONAL_PERSISTED_ROUTE_REQUIRED",
                "Native GetLayer replaces UserName with the authenticated caller. Use the independently discovered persisted XXXDef identity to acquire another owner's definition.",
            );
        }
        Scope::Personal {
            company: id.keys["Company"].clone(),
            owner: id.keys["UserName"].clone(),
        }
    } else if id.keys["Company"].is_empty() {
        Scope::Global
    } else {
        Scope::Company {
            company: id.keys["Company"].clone(),
        }
    };
    if id.scope != expected_scope {
        return fail(
            "SCOPE_MISMATCH",
            "The native MetaFX keys and declared ownership scope differ.",
        );
    }
    let phase = request
        .state
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("initial")
        .to_string();
    // Earlier checkpoints used GetVersions followed by GetLayer(version,false),
    // which reads current published content. Never resume that false source path.
    if phase == "revision"
        || (phase != "initial" && request.state["kineticReadSchema"].as_u64() != Some(3))
    {
        return fail(
            "KINETIC_CHECKPOINT_UPGRADE_REQUIRED",
            "This checkpoint predates current retained-version and child ownership checks. Restart this exact identity from its initial read; retained native receipts remain available.",
        );
    }
    if phase == "resourceFiles" {
        return crate::kinetic_resources::handle(request);
    }

    if phase == "persistedFiles" {
        return crate::kinetic_files::handle(request);
    }
    if phase == "fileSystemPolicy" {
        return crate::kinetic_files::policy_handle(request);
    }
    if phase == "initial" {
        return action(
            "Ice.Lib.MetaFXSvc",
            "GetApplications",
            json!({"request":{"Type":"","SubType":null,"SearchText":null,"IsPublished":false,"IncludeAllLayers":true,"IncludePersLayers":false}}),
            false,
            json!({"phase":"systemPolicy","kineticReadSchema":3,"_hostCaller":caller}),
        );
    }
    if phase == "systemPolicy" {
        if let Some(error) = request.error.take() {
            return GuestReply::Failure { diagnostic: error };
        }
        let mut discovery = request.clone();
        discovery.operation = "discover".into();
        discovery.cursor = None;
        let GuestReply::Inventory {
            identities,
            authoritative: true,
            ..
        } = discover(discovery)
        else {
            return fail(
                "SYSTEM_POLICY_UNVERIFIED",
                "The native application catalog could not verify this artifact.",
            );
        };
        if !identities.iter().any(|candidate| {
            if layer {
                candidate == id
            } else {
                candidate.kind == "MetaFXApplication"
                    && candidate.keys["ViewId"] == id.keys["ViewId"]
            }
        }) {
            return crate::system_artifacts::refused();
        }
        let state = json!({"phase":"versions","kineticReadSchema":3,"content":{},"errors":[],"_hostCaller":caller});
        if !layer {
            return crate::kinetic_files::policy_start(request, state);
        }
        return read_versions(request, state);
    }
    if phase == "versions" {
        // GetVersions groups by Version and discards content rows. This read
        // returns every retained native file/layer Content for exact filtering.
        let mut state = request.state.clone();
        if let Some(error) = request.error.take() {
            state["errors"].as_array_mut().unwrap().push(json!(error));
        } else {
            let mut errors = Vec::new();
            let retained = retained_versions(
                id,
                &request.company,
                request.response.as_ref().unwrap_or(&Value::Null),
                &mut errors,
            );
            state["versionInventory"] = json!({"retainedRows":retained});
            state["errors"]
                .as_array_mut()
                .unwrap()
                .extend(errors.into_iter().map(|e| json!(e)));
            // Content is already preserved. Re-reading a version with GetLayer
            // and IncludeDraftContent=false would return current published data.
        }
        if layer {
            state["phase"] = json!("published");
            return action(
                "Ice.Lib.MetaFXSvc",
                "GetLayer",
                layer_request(id, caller, false, 0),
                false,
                state,
            );
        }
        // GetApp constructs a runtime view, applies user/company context and records
        // activity. Authored backups use ExportApp plus independently measured files.
        state["phase"] = json!("export");
        return action(
            "Ice.Lib.MetaFXSvc",
            "ExportApp",
            json!({"viewId":id.keys["ViewId"]}),
            true,
            state,
        );
    }
    if matches!(phase.as_str(), "published" | "draft" | "export") {
        let mut state = request.state.clone();
        let label = phase.clone();
        if let Some(error) = request.error.take() {
            state["errors"].as_array_mut().unwrap().push(json!(error));
        } else if let Some(response) = request.response.take() {
            let mut errors = Vec::new();
            if layer {
                validate_layer_response(id, &response, &mut errors);
            }
            if phase == "export" {
                validate_app_export(id, &response, &mut errors);
            }
            state["errors"]
                .as_array_mut()
                .unwrap()
                .extend(errors.into_iter().map(|e| json!(e)));
            state["content"][&label] = response;
        }
        if phase == "published" && layer {
            state["phase"] = json!("draft");
            return action(
                "Ice.Lib.MetaFXSvc",
                "GetLayer",
                layer_request(id, caller, true, 0),
                false,
                state,
            );
        }
        if phase != "export" {
            if !layer {
                state["phase"] = json!("export");
                return action(
                    "Ice.Lib.MetaFXSvc",
                    "ExportApp",
                    json!({"viewId":id.keys["ViewId"]}),
                    true,
                    state,
                );
            }
        }
        if !layer {
            return crate::kinetic_files::start(request, state);
        }
        return finish(request, state);
    }
    fail("INVALID_PLUGIN_STATE", "The MetaFX checkpoint is invalid.")
}

pub(crate) fn read_versions(request: GuestRequest, mut state: Value) -> GuestReply {
    state["phase"] = json!("versions");
    action(
        "Ice.Lib.UxAppVersionSvc",
        "GetUxAppVersions",
        version_request(request.identity.as_ref().unwrap(), &request.company),
        false,
        state,
    )
}

pub(crate) fn finish(request: GuestRequest, state: Value) -> GuestReply {
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(state["errors"].clone()).unwrap_or_default();
    if state["content"].as_object().is_none_or(|c| c.is_empty()) {
        return fail(
            "CONTENT_UNAVAILABLE",
            "None of the native authored-content reads succeeded. Their failures remain recorded by the host.",
        );
    }
    let authored_files = state["content"]["export"]
        .get("returnObj")
        .and_then(|v| v.get("Files"))
        .and_then(Value::as_object)
        .map(|files| {
            files
                .iter()
                .filter_map(|(path, content)| {
                    content
                        .as_str()
                        .map(|text| json!({"Path":path,"Content":text}))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut text_lengths = Vec::new();
    for collection in ["persistedFiles", "resourceFiles"] {
        if let Some(rows) = state[collection].as_array() {
            for (index, _) in rows.iter().enumerate() {
                text_lengths.push(json!({"row":format!("/{collection}/{index}"),"field":"Content","lengthField":"HerbertContentBytes"}));
            }
        }
    }
    return GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "native-authored-metafx-content-json".into(),
        definition: json!({"versions":state["versionInventory"],"content":state["content"],"authoredFiles":authored_files,"persistedFiles":state["persistedFiles"],"resourceFiles":state["resourceFiles"],"resourceReferences":state["resourceReferences"],"resourceClosureComplete":state["resourceClosureComplete"],"fileCoverage":state["fileCoverage"],"herbertSources":{"textLengths":text_lengths,"chunkStreams":[]}}),
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    };
}

fn version_request(id: &Identity, selected_company: &str) -> Value {
    let layer = id.kind == "MetaFXLayer";
    // An empty Company in GetUxAppVersions means every company. Supply the
    // selected company, which natively includes global rows, then filter keys.
    // Native IsBase is Key2.IsNullOrEmpty (UxAppVersionSvc.cs:529-531). Its
    // base predicate (line 76) explicitly includes Key1=view OR
    // (Key1 empty AND Key2=view), so this one request includes event layouts.
    json!({"request":{
        "Company":selected_company,"ProductID":"EP","TypeCode":if layer {&id.keys["TypeCode"]} else {""},
        "Key1":if layer {&id.keys["LayerName"]} else {&id.keys["ViewId"]},
        "Key2":if layer {&id.keys["ViewId"]} else {""},
        "Key3":if layer {&id.keys["DeviceType"]} else {""},
        "CGCCode":if layer {&id.keys["CGCCode"]} else {""},"Version":0
    }})
}
fn retained_versions(
    id: &Identity,
    selected_company: &str,
    response: &Value,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Value> {
    let Some(rows) = body(response).as_array() else {
        diagnostics.push(Diagnostic::new(
            "VERSION_INVENTORY_UNVERIFIED",
            "The retained version route omitted its native row array.",
        ));
        return vec![];
    };
    let layer = id.kind == "MetaFXLayer";
    let mut seen = BTreeSet::new();
    let mut retained = Vec::new();
    for row in rows {
        let fields = [
            "Company",
            "ProductID",
            "TypeCode",
            "Key1",
            "Key2",
            "Key3",
            "CGCCode",
        ];
        if fields
            .iter()
            .any(|key| row.get(*key).and_then(Value::as_str).is_none())
            || row
                .get("Version")
                .and_then(Value::as_i64)
                .is_none_or(|n| n <= 0 || n > i32::MAX as i64)
            || !row
                .get("Content")
                .is_some_and(|v| v.is_string() || v.is_null())
        {
            diagnostics.push(Diagnostic::new("VERSION_IDENTITY_UNVERIFIED", "A retained version row lacks its complete native key, Int32 version, or Content field."));
            continue;
        }
        let get = |field: &str| row[field].as_str().unwrap();
        if !get("Company").is_empty() && get("Company") != selected_company {
            diagnostics.push(Diagnostic::new(
                "VERSION_SCOPE_MISMATCH",
                "The native version route returned a row outside the selected company.",
            ));
            continue;
        }
        let matches = if layer {
            get("Company") == id.keys["Company"]
                && get("ProductID") == "EP"
                && get("TypeCode") == id.keys["TypeCode"]
                && get("Key1") == id.keys["LayerName"]
                && get("Key2").eq_ignore_ascii_case(&id.keys["ViewId"])
                && get("Key3") == id.keys["DeviceType"]
                && get("CGCCode") == id.keys["CGCCode"]
        } else {
            get("ProductID") == "EP"
                && ((get("TypeCode") == "KNTCMetaUI"
                    && get("Key1").eq_ignore_ascii_case(&id.keys["ViewId"]))
                    || (get("TypeCode") == "KNTCEventLayout"
                        && get("Key1").is_empty()
                        && get("Key2").eq_ignore_ascii_case(&id.keys["ViewId"])))
        };
        // The native predicate deliberately uses Key2.Contains and optional
        // Company/CGC filters. Raw acquisition remains preserved by the host.
        // Only the exact source relationship belongs to this derived artifact.
        if !matches {
            continue;
        }
        let key = serde_json::to_string(&json!([
            row["Company"],
            row["ProductID"],
            row["TypeCode"],
            row["Key1"],
            row["Key2"],
            row["Key3"],
            row["CGCCode"],
            row["Version"]
        ]))
        .unwrap();
        if !seen.insert(key) {
            diagnostics.push(Diagnostic::new(
                "VERSION_IDENTITY_DUPLICATE",
                "The retained version response repeats a complete native version key.",
            ));
            continue;
        }
        retained.push(row.clone());
    }
    retained
}
fn validate_layer_response(id: &Identity, response: &Value, diagnostics: &mut Vec<Diagnostic>) {
    let layer = body(response);
    let expected = [
        "ViewId",
        "Company",
        "TypeCode",
        "LayerName",
        "DeviceType",
        "CGCCode",
    ];
    if expected
        .iter()
        .any(|key| layer.get(*key).and_then(Value::as_str) != id.keys.get(*key).map(String::as_str))
        || !layer.get("Content").is_some_and(Value::is_object)
    {
        diagnostics.push(Diagnostic::new(
            "LAYER_CONTENT_IDENTITY_UNVERIFIED",
            "The native layer content does not match the complete requested layer identity.",
        ));
    }
    if personal(&id.keys["TypeCode"])
        && layer.get("UserName").and_then(Value::as_str) != Some(id.keys["UserName"].as_str())
    {
        diagnostics.push(Diagnostic::new("PERSONAL_OWNER_UNVERIFIED", "The native GetLayer route replaces UserName with the authenticated caller; acquire other owners through the exact persisted definition route."));
    }
}
fn validate_app_export(id: &Identity, response: &Value, diagnostics: &mut Vec<Diagnostic>) {
    let export = body(response);
    if export.get("ViewId").and_then(Value::as_str) != Some(id.keys["ViewId"].as_str()) {
        diagnostics.push(Diagnostic::new("APP_EXPORT_IDENTITY_UNVERIFIED", "The native export ViewId does not match the requested app. Configurator exports require their documented ConfigId mapping."));
    }
    if !export
        .get("Files")
        .and_then(Value::as_object)
        .is_some_and(|files| !files.is_empty() && files.values().all(Value::is_string))
    {
        diagnostics.push(Diagnostic::new(
            "APP_EXPORT_CONTENT_UNVERIFIED",
            "The native app export lacks its authored Files map of text values.",
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> Identity {
        Identity {
            category: "C12".into(),
            kind: "MetaFXApplication".into(),
            scope: Scope::Company {
                company: "EXAMPLE".into(),
            },
            keys: BTreeMap::from([
                ("ViewId".into(), "Ice.UIDbd.Example".into()),
                ("Company".into(), "EXAMPLE".into()),
                ("Type".into(), "view".into()),
                ("SubType".into(), "Dashboard".into()),
                ("DeviceType".into(), "Desktop".into()),
            ]),
        }
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "discover".into(),
            category: "C12".into(),
            company: "EXAMPLE".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn system_app_keeps_custom_layers_but_not_system_layers() {
        let mut r = request();
        let layer = json!({"Company":"EXAMPLE","TypeCode":"KNTCCustLayer","LayerName":"custom","DeviceType":"Desktop","SystemFlag":false});
        let mut system_layer = layer.clone();
        system_layer["LayerName"] = json!("stock");
        system_layer["SystemFlag"] = json!(true);
        r.response =
            Some(json!({"returnObj":[{"Id":"A","SystemFlag":true,"Layers":[layer,system_layer]}]}));
        let GuestReply::Inventory {
            identities,
            authoritative,
            ..
        } = discover(r)
        else {
            panic!()
        };
        assert!(authoritative);
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["LayerName"], "custom");
    }
    #[test]
    fn explicit_system_app_cannot_reach_export() {
        let mut r = request();
        r.operation = "acquire".into();
        r.identity = Some(app());
        r.state = json!({"phase":"systemPolicy","kineticReadSchema":3,"_hostCaller":"manager"});
        r.response = Some(json!({"returnObj":[{"Id":"Ice.UIDbd.Example","SystemFlag":true}]}));
        assert!(
            matches!(handle(r),GuestReply::Failure { diagnostic } if diagnostic.code == "SYSTEM_ARTIFACT_EXCLUDED")
        );
    }
    #[test]
    fn native_flat_apps_are_valid_and_customization_branch_is_selected() {
        let GuestReply::Action {
            action: HostAction::SharedNativeRead { arguments, .. },
            ..
        } = discover(request())
        else {
            panic!()
        };
        assert_eq!(arguments["request"]["IncludePersLayers"], false);
        assert_eq!(arguments["request"]["IsPublished"], false);
        let mut r = request();
        r.response = Some(json!({"returnObj":[{"Id":"A","Type":"view","SubType":"Apps"}]}));
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            next_cursor,
        } = discover(r)
        else {
            panic!()
        };
        assert_eq!(identities.len(), 1);
        assert!(authoritative);
        assert!(diagnostics.is_empty());
        assert!(next_cursor.is_none());
    }
    #[test]
    fn apps_acquire_authored_export_without_opening_a_runtime_view() {
        let mut r = request();
        r.operation = "acquire".into();
        r.identity = Some(app());
        r.state = json!({"phase":"versions","kineticReadSchema":3,"content":{},"errors":[],"_hostCaller":"manager"});
        r.response = Some(json!({"returnObj":[]}));
        let GuestReply::Action {
            action:
                HostAction::SharedNativeRead {
                    service,
                    method,
                    arguments,
                    export,
                },
            state,
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(service, "Ice.Lib.MetaFXSvc");
        assert_eq!(method, "ExportApp");
        assert_eq!(arguments["viewId"], "Ice.UIDbd.Example");
        assert!(export);
        assert_eq!(state["phase"], "export");
        assert!(!methods().iter().any(|(_, method)| method == "GetApp"));
        let version = version_request(&app(), "EXAMPLE");
        assert_eq!(version["request"]["Company"], "EXAMPLE");
        assert_eq!(version["request"]["Key2"], "");
    }
    #[test]
    fn retained_file_content_and_complete_version_key_are_preserved() {
        let row = json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"Ice.UIDbd.Example","Key2":"Ice.UIDbd.Example\\layout.draft.jsonc","Key3":"","CGCCode":"","Version":3,"Content":"original bytes"});
        let response = json!({"returnObj":[row.clone()]});
        let mut errors = Vec::new();
        assert_eq!(
            retained_versions(&app(), "EXAMPLE", &response, &mut errors),
            vec![row.clone()]
        );
        assert!(errors.is_empty());
        let duplicated = json!({"returnObj":[row.clone(),row]});
        retained_versions(&app(), "EXAMPLE", &duplicated, &mut errors);
        assert!(
            errors
                .iter()
                .any(|e| e.code == "VERSION_IDENTITY_DUPLICATE")
        );
    }
    #[test]
    fn native_personal_codes_and_export_content_are_checked() {
        assert!(personal("KNTCPersLayer"));
        assert!(personal("KineticGridLayout"));
        assert!(!personal("KNTCCustLayer"));
        let mut errors = Vec::new();
        validate_app_export(
            &app(),
            &json!({"returnObj":{"ViewId":"Ice.UIDbd.Example","Files":{"layout.jsonc":"{ /* native JSONC preserved */ }"}}}),
            &mut errors,
        );
        assert!(errors.is_empty());
        validate_app_export(
            &app(),
            &json!({"returnObj":{"ViewId":"other","Files":{}}}),
            &mut errors,
        );
        assert_eq!(errors.len(), 2);
    }

    fn personal_layer() -> Identity {
        Identity {
            category: "C13".into(),
            kind: "MetaFXLayer".into(),
            scope: Scope::Personal {
                company: "EXAMPLE".into(),
                owner: "alice".into(),
            },
            keys: BTreeMap::from([
                ("ViewId".into(), "Ice.UIDbd.Example".into()),
                ("Company".into(), "EXAMPLE".into()),
                ("TypeCode".into(), "KNTCPersLayer".into()),
                ("LayerName".into(), "alice".into()),
                ("DeviceType".into(), "Desktop".into()),
                ("CGCCode".into(), "".into()),
                ("UserName".into(), "alice".into()),
            ]),
        }
    }
    #[test]
    fn base_version_request_includes_native_empty_key_event_layout_branch() {
        let request = version_request(&app(), "EXAMPLE");
        // Native ListAllVersion's isBase branch accepts this row because
        // Key2 in the request is empty and row.Key1 is empty/row.Key2=request.Key1.
        assert_eq!(request["request"]["Key2"], "");
        assert_eq!(request["request"]["TypeCode"], "");
        let event = json!({"Company":"","ProductID":"EP","TypeCode":"KNTCEventLayout","Key1":"","Key2":request["request"]["Key1"],"Key3":"","CGCCode":"","Version":4,"Content":"saved event layout"});
        let mut errors = Vec::new();
        assert_eq!(
            retained_versions(
                &app(),
                "EXAMPLE",
                &json!({"returnObj":[event.clone()]}),
                &mut errors
            ),
            vec![event]
        );
        assert!(errors.is_empty());
    }
    #[test]
    fn legacy_revision_checkpoints_never_dispatch_false_version_reads() {
        for state in [
            json!({"phase":"revision","kineticReadSchema":3,"currentVersion":7}),
            json!({"phase":"published","versions":[7]}),
        ] {
            let mut r = request();
            r.operation = "acquire".into();
            r.identity = Some(app());
            r.state = state;
            r.state["_hostCaller"] = json!("manager");
            let GuestReply::Failure { diagnostic } = handle(r) else {
                panic!("legacy source path dispatched")
            };
            assert_eq!(diagnostic.code, "KINETIC_CHECKPOINT_UPGRADE_REQUIRED");
        }
    }
    #[test]
    fn other_personal_owner_and_forged_scope_are_refused_before_native_read() {
        let mut r = request();
        r.operation = "acquire".into();
        r.category = "C13".into();
        r.identity = Some(personal_layer());
        r.state = json!({"_hostCaller":"manager"});
        let GuestReply::Failure { diagnostic } = handle(r.clone()) else {
            panic!("other owner dispatched")
        };
        assert_eq!(diagnostic.code, "PERSONAL_PERSISTED_ROUTE_REQUIRED");
        r.state["_hostCaller"] = json!("alice");
        r.identity.as_mut().unwrap().scope = Scope::Company {
            company: "EXAMPLE".into(),
        };
        let GuestReply::Failure { diagnostic } = handle(r) else {
            panic!("forged scope dispatched")
        };
        assert_eq!(diagnostic.code, "SCOPE_MISMATCH");
        let id = personal_layer();
        let mut response = json!(id.keys);
        response["UserName"] = json!("manager");
        response["Content"] = json!({});
        let mut errors = vec![];
        validate_layer_response(&id, &response, &mut errors);
        assert!(errors.iter().any(|e| e.code == "PERSONAL_OWNER_UNVERIFIED"));
    }
}
