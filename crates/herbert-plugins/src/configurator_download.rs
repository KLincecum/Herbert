//! One configurator export supplies its definition, design, code, and app.
//! Inventory retains attachments; dynamic-list expressions are not in the export.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
fn fail(message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new("CONFIGURATOR_DOWNLOAD_INVALID", message),
    }
}
fn record(r: &mut GuestRequest, d: Diagnostic) {
    r.state["errors"].as_array_mut().unwrap().push(json!(d));
}
fn finish(r: GuestRequest, spec: &super::Family) -> GuestReply {
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(r.state["errors"].clone()).unwrap_or_default();
    let mut definition = json!({"service":spec.service,"definition":r.state["definition"],"supplements":r.state["supplements"]});
    super::configurator_export::attach_sources(&mut definition);
    if let Some(text) = r.state["localXml"].as_str() {
        if !definition["herbertSources"]["files"].is_array() {
            definition["herbertSources"]["files"] = json!([]);
        }
        definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({
            "name":format!("{}.xml",crate::bpm_code::label(&r.identity.as_ref().unwrap().keys["ConfigID"])),
            "text":text
        }));
    }
    if r.state["localZip"].is_object() {
        definition["herbertSources"]["binaryFiles"] = json!([r.state["localZip"]]);
    }
    GuestReply::Definition {
        identity: r.identity.unwrap(),
        format: "native-definition-and-supplements-json".into(),
        definition,
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    }
}
fn expressions(mut r: GuestRequest, spec: &super::Family) -> GuestReply {
    if r.state["phase"] == "configExpressions" {
        if let Some(error) = r.error.take() {
            record(&mut r, error);
            return finish(r, spec);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        let Some(rows) = response["rows"].as_array() else {
            return fail("The dynamic-list expression page omitted its rows.");
        };
        let id = r.identity.as_ref().unwrap();
        for row in rows {
            if row["Company"] != id.keys["Company"] {
                return fail("An expression belongs to another company.");
            }
            if row["ConfigID"] == id.keys["ConfigID"] {
                r.state["definition"]["PcDynLstExpr"]
                    .as_array_mut()
                    .unwrap()
                    .push(row.clone());
            }
        }
        if rows.len() < r.page_size.unwrap() as usize {
            return finish(r, spec);
        }
        let after = rows.last().unwrap()["SysRowID"].clone();
        if !after.is_string() || after == r.state["after"] {
            return fail("The expression page did not advance.");
        }
        r.state["after"] = after;
    } else {
        r.state["phase"] = json!("configExpressions");
        r.state["definition"]["PcDynLstExpr"] = json!([]);
        r.state["after"] = Value::Null;
    }
    let mut parameters = BTreeMap::from([(
        "company".into(),
        json!(r.identity.as_ref().unwrap().keys["Company"]),
    )]);
    let mut criteria = vec![QueryCriterion {
        table: "Expression".into(),
        field: "Company".into(),
        operator: "=".into(),
        parameter: "company".into(),
    }];
    if !r.state["after"].is_null() {
        parameters.insert("after".into(), r.state["after"].clone());
        criteria.push(QueryCriterion {
            table: "Expression".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    GuestReply::Action {
        action: HostAction::SharedQuery {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Erp".into(),
                    table: "PcDynLstExpr".into(),
                    alias: "Expression".into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields: vec![QueryField {
                    table: "Expression".into(),
                    field: "*".into(),
                    alias: "*".into(),
                    expression: None,
                }],
                joins: vec![],
                criteria,
                parameters,
                order: vec![QueryOrder {
                    field: "SysRowID".into(),
                    descending: false,
                }],
                page_size: r.page_size,
                page: 1,
            },
        },
        state: r.state,
    }
}
fn next(mut r: GuestRequest, spec: &super::Family) -> GuestReply {
    if r.state["supplements"]["StoredConfigurator"].is_object()
        && r.state["signatureChecked"] != true
    {
        r.state["phase"] = json!("configSignature");
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetLicensedModuleAccess".into(),
                arguments: json!({}),
                export: false,
            },
            state: r.state,
        };
    }
    let stored = &r.state["supplements"]["StoredConfigurator"];
    if stored.is_object()
        && r.state["signatureRequired"] != true
        && stored["PcRuleSet"].as_array().is_some_and(Vec::is_empty)
    {
        match crate::configurator_archive::xml(stored, &BTreeMap::new()) {
            Ok(xml) => {
                let attachments = r.state["definition"]["PcStatusAttch"].clone();
                r.state["definition"] = stored.clone();
                r.state["definition"]["PcStatusAttch"] = attachments;
                r.state["localXml"] = json!(xml);
                if r.state["signatureRequired"] != false {
                    record(
                        &mut r,
                        Diagnostic::new(
                            "CONFIGURATOR_SIGNATURE_UNVERIFIED",
                            "Local XML and original source are retained; native signature applicability has not been established for this environment.",
                        ),
                    );
                }
                if r.state["definition"]["PcStatus"][0]["Kinetic"] == true {
                    if r.state["supplements"]["StoredApp"].is_object() {
                        let id = r.identity.as_ref().unwrap();
                        match crate::configurator_app::from_visible(
                            &id.keys["Company"],
                            &id.keys["ConfigID"],
                            &r.state["supplements"]["StoredApp"],
                        ) {
                            Ok(app) => {
                                r.state["supplements"]["ConfiguratorCurrentFiles"] = json!({"files":app["Files"],"rows":r.state["supplements"]["StoredApp"]["rows"]});
                                r.state["supplements"]["LocalAppContent"] = app;
                                r.state["configuratorAppPolicyAllowed"] = json!(true);
                                r.state["configuratorFilesComplete"] = json!(true);
                                return finish_local_kinetic(r, spec);
                            }
                            Err(error) => {
                                // This is a local-route decision, not loss of the
                                // retained source. The native fallback still runs.
                                r.state["supplements"]["LocalAppFallback"] = json!(error);
                            }
                        }
                    }
                    let action =
                        super::configurator_export::policy_start(&mut r, "configLocalPolicy");
                    return GuestReply::Action {
                        action,
                        state: r.state,
                    };
                }
                return finish(r, spec);
            }
            Err(error) => record(&mut r, error),
        }
    }
    if let Some(action) = super::configurator_export::start(&mut r) {
        return GuestReply::Action {
            action,
            state: r.state,
        };
    }
    // A mixed-System app cannot use its whole-app exporter. Retain the custom
    // database definition and filtered app files through the native read path.
    r.state["phase"] = json!("configFallback");
    GuestReply::Action {
        action: HostAction::NativeRead {
            service: spec.service.clone(),
            method: "GetByID".into(),
            arguments: json!({"configID":r.identity.as_ref().unwrap().keys["ConfigID"]}),
            export: false,
        },
        state: r.state,
    }
}
fn finish_local_kinetic(mut r: GuestRequest, spec: &super::Family) -> GuestReply {
    if r.state["configuratorAppPolicyAllowed"] == true
        && r.state["configuratorFilesComplete"] == true
        && r.state["errors"].as_array().is_some_and(Vec::is_empty)
    {
        let mut app = if r.state["supplements"]["LocalAppContent"].is_object() {
            r.state["supplements"]["LocalAppContent"].clone()
        } else {
            r.state["supplements"]["NativeAppContent"].clone()
        };
        app["Files"] = r.state["supplements"]["ConfiguratorCurrentFiles"]["files"].clone();
        match crate::configurator_archive::kinetic_package(
            &r.state["supplements"]["StoredConfigurator"],
            &BTreeMap::new(),
            &app,
        ) {
            Ok(bytes) => {
                r.state["localZip"] = json!({"name":format!("{}.zip",crate::bpm_code::label(&r.identity.as_ref().unwrap().keys["ConfigID"])),"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"base64":base64::engine::general_purpose::STANDARD.encode(&bytes)})
            }
            Err(error) => record(&mut r, error),
        }
    }
    finish(r, spec)
}
pub(super) fn handle(mut r: GuestRequest, spec: &super::Family) -> GuestReply {
    let phase = r.state["phase"].as_str().unwrap_or("initial").to_owned();
    if phase == "configLocalPolicy" {
        if let Some(action) = super::configurator_export::policy_complete(&mut r) {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        if r.state["configuratorAppPolicyAllowed"] != true {
            if let Some(action) =
                super::configurator_export::files_start(&mut r, "configLocalFiles")
            {
                return GuestReply::Action {
                    action,
                    state: r.state,
                };
            }
            return finish_local_kinetic(r, spec);
        }
        r.state["phase"] = json!("configLocalApp");
        let id = r.identity.as_ref().unwrap();
        return GuestReply::Action {
            action: HostAction::NativeRead {
                service: "Erp.BO.ErpMetaFxSvc".into(),
                method: "ExportApp".into(),
                arguments: json!({"viewId":format!("Erp.UICfg.{}_{}",id.keys["ConfigID"],id.keys["Company"])}),
                export: true,
            },
            state: r.state,
        };
    }
    if phase == "configLocalApp" {
        if let Some(error) = r.error.take() {
            record(&mut r, error);
            return finish(r, spec);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        let app = response["returnObj"].clone();
        // Validate the native envelope before accepting any filenames or bodies.
        if let Err(error) = crate::configurator_archive::validate_kinetic_app(
            &r.state["supplements"]["StoredConfigurator"],
            &app,
        ) {
            record(&mut r, error);
            return finish(r, spec);
        }
        r.state["nativeAppFiles"] = app["Files"].clone();
        r.state["supplements"]["ConfiguratorCurrentFiles"] =
            json!({"files":app["Files"],"rows":[]});
        r.state["supplements"]["NativeAppContent"] = app;
        if let Some(action) = super::configurator_export::files_start(&mut r, "configLocalFiles") {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return finish_local_kinetic(r, spec);
    }
    if phase == "configLocalFiles" {
        if let Some(action) = super::configurator_export::files_complete(&mut r) {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return finish_local_kinetic(r, spec);
    }
    if phase == "configSignature" {
        r.state["signatureChecked"] = json!(true);
        if let Some(error) = r.error.take() {
            record(&mut r, error);
        } else {
            let response = r.response.take().unwrap_or(Value::Null);
            r.state["signatureRequired"] =
                json!(crate::configurator_pages::multi_tenant(&response));
            r.state["supplements"]["SignatureCapability"] = response;
        }
        return next(r, spec);
    }
    if phase == "initial" {
        let data = r
            .state
            .pointer("/_inventory/response/returnObj")
            .cloned()
            .unwrap_or(Value::Null);
        let Some(rows) = data["PcStatus"].as_array().filter(|rows| rows.len() == 1) else {
            return fail("The configurator inventory did not retain its export settings.");
        };
        if super::make_identity(spec, &rows[0], &r.company)
            .ok()
            .as_ref()
            != r.identity.as_ref()
        {
            return fail("The configurator inventory has a different identity.");
        }
        let stored = r.state.pointer("/_inventory/storedConfigurator").cloned();
        if let Some(stored) = &stored {
            let Some(stored_roots) = stored["PcStatus"].as_array().filter(|rows| rows.len() == 1)
            else {
                return fail("The stored configurator graph omitted its unique root.");
            };
            if super::make_identity(spec, &stored_roots[0], &r.company)
                .ok()
                .as_ref()
                != r.identity.as_ref()
                || stored_roots[0]["SysRowID"] != rows[0]["SysRowID"]
            {
                return fail("The stored configurator graph differs from its visible identity.");
            }
        }
        let stored_app = r.state.pointer("/_inventory/storedApp").cloned();
        r.state = json!({"phase":"configImages","definition":data,"supplements":{},"errors":[]});
        if let Some(stored) = stored {
            r.state["supplements"]["StoredConfigurator"] = stored;
        }
        if let Some(app) = stored_app {
            r.state["supplements"]["StoredApp"] = app;
        }
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: spec.service.clone(),
                method: "RetrievePcLayeredImagesForExport".into(),
                arguments: json!({"configID":r.identity.as_ref().unwrap().keys["ConfigID"],"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[],"ExtensionTables":[]}}),
                export: false,
            },
            state: r.state,
        };
    }
    if phase == "configImages" {
        if let Some(d) = r.error.take() {
            record(&mut r, d);
            return finish(r, spec);
        }
        r.state["supplements"]["RetrievePcLayeredImagesForExport"] =
            r.response.take().unwrap_or(Value::Null);
        return next(r, spec);
    }
    if phase == "configuratorExportPolicy" {
        if let Some(action) = super::configurator_export::policy_complete(&mut r) {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return next(r, spec);
    }
    if phase == "configuratorExport" {
        super::configurator_export::complete(&mut r);
        let export = &r.state["supplements"]["ExportConfiguration"]["returnObj"];
        let primary = export["nativeFile"].clone();
        let xml = export["nativeFiles"]
            .as_array()
            .and_then(|files| {
                files
                    .iter()
                    .find(|f| f["path"].as_str().is_some_and(|p| p.ends_with(".xml")))
            })
            .cloned()
            .unwrap_or(primary.clone());
        let Ok(file) = serde_json::from_value::<FileRef>(xml) else {
            return finish(r, spec);
        };
        r.state["xmlFile"] = json!(file);
        r.state["appFile"] = primary;
        r.state["phase"] = json!("configXml");
        return GuestReply::Action {
            action: HostAction::ParseXml { file },
            state: r.state,
        };
    }
    if phase == "configXml" {
        if let Some(d) = r.error.take() {
            record(&mut r, d);
            return finish(r, spec);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if response["sourceFile"] != r.state["xmlFile"] {
            return fail("The parsed XML differs from the downloaded file.");
        }
        let parsed = match crate::xml_dataset::parse_flat(&response["parsedXml"]) {
            Ok(data) => data,
            Err(d) => {
                record(&mut r, d);
                return finish(r, spec);
            }
        };
        let mut data = json!(parsed.tables);
        let Some(rows) = data["PcStatus"].as_array().filter(|rows| rows.len() == 1) else {
            return fail("The configurator export must contain one root.");
        };
        if super::make_identity(spec, &rows[0], &r.company)
            .ok()
            .as_ref()
            != r.identity.as_ref()
        {
            return fail("The configurator export has a different identity.");
        }
        data["PcStatusAttch"] = r.state["definition"]["PcStatusAttch"].clone();
        r.state["definition"] = data;
        if r.state["configuratorExportKinetic"] == true {
            let Ok(file) = serde_json::from_value::<FileRef>(r.state["appFile"].clone()) else {
                return fail("The Kinetic app package is unavailable.");
            };
            r.state["phase"] = json!("configApp");
            return GuestReply::Action {
                action: HostAction::ParseJsonArchive {
                    file,
                    entries: vec![format!(
                        "{}.jsonc",
                        r.identity.as_ref().unwrap().keys["ConfigID"]
                    )],
                },
                state: r.state,
            };
        }
        return expressions(r, spec);
    }
    if phase == "configApp" {
        if let Some(d) = r.error.take() {
            record(&mut r, d);
            return finish(r, spec);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if response["sourceFile"] != r.state["appFile"] {
            return fail("The parsed app differs from the downloaded package.");
        }
        let name = format!("{}.jsonc", r.identity.as_ref().unwrap().keys["ConfigID"]);
        let app = &response["parsedJsonArchive"]["documents"][&name]["json"];
        let id = r.identity.as_ref().unwrap();
        let view = format!("Erp.UICfg.{}_{}", id.keys["ConfigID"], id.keys["Company"]);
        if !(app["ViewId"] == id.keys["ConfigID"] || app["ViewId"] == view)
            || !(app["CompanyId"].is_null() || app["CompanyId"] == id.keys["Company"])
        {
            return fail("The exported app has a different configurator identity.");
        }
        let Some(files) = app["Files"].as_object() else {
            return fail("The exported app has no file map.");
        };
        if files.iter().any(|(name, text)| {
            !text.is_string()
                || name.is_empty()
                || name.contains(['\\', ':'])
                || name.starts_with('/')
                || name.split('/').any(|p| p == ".." || p.is_empty())
        }) {
            return fail("The exported app has an invalid filename or text value.");
        }
        r.state["nativeAppFiles"] = json!(files);
        r.state["supplements"]["ConfiguratorCurrentFiles"] = json!({"files":files,"rows":[]});
        if let Some(action) = super::configurator_export::files_start(&mut r, "configMissingFiles")
        {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return expressions(r, spec);
    }
    if phase == "configFallback" {
        if let Some(d) = r.error.take() {
            record(&mut r, d);
            return finish(r, spec);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        let data = response["returnObj"].clone();
        let Some(rows) = data["PcStatus"].as_array().filter(|rows| rows.len() == 1) else {
            return fail("The fallback configurator read omitted its root.");
        };
        if super::make_identity(spec, &rows[0], &r.company)
            .ok()
            .as_ref()
            != r.identity.as_ref()
        {
            return fail("The fallback configurator has a different identity.");
        }
        r.state["definition"] = data;
        r.state["phase"] = json!("configFallbackDesign");
        return GuestReply::Action {
            action: HostAction::NativeRead {
                service: "Erp.BO.ConfigurationDesignSvc".into(),
                method: "GetByID".into(),
                arguments: json!({"configID":r.identity.as_ref().unwrap().keys["ConfigID"]}),
                export: false,
            },
            state: r.state,
        };
    }
    if phase == "configFallbackDesign" {
        if let Some(d) = r.error.take() {
            record(&mut r, d);
        } else {
            let response = r.response.take().unwrap_or(Value::Null);
            let data = &response["returnObj"];
            let Some(rows) = data["PcStatus"].as_array().filter(|rows| rows.len() == 1) else {
                return fail("The fallback design read omitted its root.");
            };
            if super::make_identity(spec, &rows[0], &r.company)
                .ok()
                .as_ref()
                != r.identity.as_ref()
            {
                return fail("The fallback design has a different identity.");
            }
            // The whole-app export is unavailable here. Its design tables need
            // one native read; ordinary exports already contain these tables.
            for (table, rows) in data.as_object().unwrap() {
                if table != "PcStatus" {
                    r.state["definition"][table] = rows.clone();
                }
            }
        }
        if let Some(action) = super::configurator_export::files_start(&mut r, "configMissingFiles")
        {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return expressions(r, spec);
    }
    if phase == "configMissingFiles" {
        if let Some(action) = super::configurator_export::files_complete(&mut r) {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return expressions(r, spec);
    }
    if phase == "configExpressions" {
        return expressions(r, spec);
    }
    fail("The configurator download checkpoint is invalid.")
}

#[cfg(test)]
mod local_kinetic_tests {
    use super::*;
    #[test]
    fn supplied_kinetic_acquisition_builds_locally_and_preserves_partial_failures() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let read = |p: &str| -> Value {
                serde_json::from_slice(&std::fs::read(folder.join(p)).unwrap()).unwrap()
            };
            let stored = read("stored/kinetic/rows.json");
            let app = read("oracle/kinetic/app.json");
            for scenario in [
                "complete",
                "foreign-app",
                "file-failure",
                "system-child",
                "bulk-complete",
                "bulk-stale",
            ] {
                let identity = Identity {
                    category: "C19".into(),
                    kind: "Configurator".into(),
                    scope: Scope::Company {
                        company: "TESTCO".into(),
                    },
                    keys: BTreeMap::from([
                        ("Company".into(), "TESTCO".into()),
                        ("ConfigID".into(), "H19K1024A".into()),
                    ]),
                };
                let mut r = GuestRequest {
                    abi: PLUGIN_ABI,
                    operation: "acquire".into(),
                    category: "C19".into(),
                    company: "TESTCO".into(),
                    identity: Some(identity),
                    cursor: None,
                    page_size: Some(100),
                    state: json!({"_inventory":{"response":{"returnObj":{"PcStatus":stored["PcStatus"],"PcStatusAttch":[]}},"storedConfigurator":stored}}),
                    response: None,
                    error: None,
                };
                if scenario.starts_with("bulk-") {
                    let visible = read("app-visible-list/responses.json");
                    let native = read("app-bulk-native/responses.json");
                    r.state["_inventory"]["storedApp"] = json!({"complete":true,"rows":native[4]["returnObj"]["Results"],"systemRows":[],"visibleRows":visible.as_array().unwrap().iter().flat_map(|p|p["returnObj"]["XXXDefList"].as_array().unwrap().iter().cloned()).collect::<Vec<_>>()});
                    if scenario == "bulk-stale" {
                        r.state["_inventory"]["storedApp"]["visibleRows"][0]["SysRevID"] = json!(0);
                    }
                }
                let mut app_reads = 0;
                loop {
                    match crate::collectors::handle(r.clone()) {
                        GuestReply::Action { action, state } => {
                            r.state = state;
                            r.error = None;
                            r.response = Some(
                                match action {
                                    HostAction::SharedNativeRead { method, .. }
                                        if method == "RetrievePcLayeredImagesForExport" =>
                                    {
                                        json!({"parameters":{"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[]}}})
                                    }
                                    HostAction::SharedNativeRead { method, .. }
                                        if method == "GetLicensedModuleAccess" =>
                                    {
                                        read(
                                            "signature-capability/001-GetLicensedModuleAccess.json",
                                        )["value"]
                                            .clone()
                                    }
                                    HostAction::SharedQuery { query } => {
                                        assert_ne!(
                                            scenario, "bulk-complete",
                                            "Verified bulk app must not repeat per-root queries"
                                        );
                                        let policy =
                                            query.fields.iter().all(|f| f.field != "Content");
                                        if policy && scenario == "system-child" {
                                            json!({"rows":[{"SystemFlag":true,"SysRowID":"system","TypeCode":"KNTCMetaUI","Key1":"Erp.UICfg.H19K1024A_TESTCO"}]})
                                        } else if !policy && scenario == "file-failure" {
                                            r.error = Some(Diagnostic::new(
                                                "TEST_FILES_FAILED",
                                                "Fixture file read failed",
                                            ));
                                            Value::Null
                                        } else {
                                            json!({"rows":[]})
                                        }
                                    }
                                    HostAction::NativeRead { method, .. }
                                        if method == "ExportApp" =>
                                    {
                                        app_reads += 1;
                                        let mut value = app.clone();
                                        if scenario == "foreign-app" {
                                            value["ViewId"] = json!("Other");
                                        }
                                        json!({"returnObj":value})
                                    }
                                    other => {
                                        panic!("Unexpected Kinetic acquisition action {other:?}")
                                    }
                                },
                            );
                        }
                        GuestReply::Definition {
                            definition,
                            completeness,
                            diagnostics,
                            ..
                        } => {
                            assert_eq!(definition["supplements"]["StoredConfigurator"], stored);
                            if matches!(scenario, "complete" | "bulk-complete" | "bulk-stale") {
                                assert_eq!(completeness, Completeness::Collected);
                                assert!(diagnostics.is_empty());
                                let zip = &definition["herbertSources"]["binaryFiles"][0];
                                assert_eq!(zip["name"], "H19K1024A.zip");
                                let bytes = base64::engine::general_purpose::STANDARD
                                    .decode(zip["base64"].as_str().unwrap())
                                    .unwrap();
                                assert_eq!(
                                    bytes,
                                    crate::configurator_archive::kinetic_package(
                                        &stored,
                                        &BTreeMap::new(),
                                        &app
                                    )
                                    .unwrap()
                                );
                            } else {
                                assert_eq!(completeness, Completeness::Partial);
                                assert!(!diagnostics.is_empty());
                                assert!(definition["herbertSources"]["binaryFiles"].is_null());
                            }
                            assert_eq!(
                                app_reads,
                                if matches!(scenario, "system-child" | "bulk-complete") {
                                    0
                                } else {
                                    1
                                }
                            );
                            break;
                        }
                        other => panic!("Unexpected reply {other:?}"),
                    }
                }
            }
        }
    }
}
