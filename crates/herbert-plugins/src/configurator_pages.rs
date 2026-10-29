//! Bulk stored rows for native-visible configurator pages.
use herbert_contract::*;
use serde_json::{Value, json};

pub fn pending(r: &GuestRequest) -> bool {
    r.state["configuratorPage"] == true
}

pub fn begin(r: GuestRequest, reply: GuestReply) -> GuestReply {
    let GuestReply::InventoryData { records, inventory } = reply else {
        return reply;
    };
    if records.is_empty() || records.iter().any(|(id, _)| id.kind != "Configurator") {
        return GuestReply::InventoryData { records, inventory };
    }
    let roots: Vec<Value> = records
        .iter()
        .map(|(_, data)| data["response"]["returnObj"]["PcStatus"][0].clone())
        .collect();
    let Some(size) = r.page_size else {
        return crate::failure("CONFIGURATOR_PAGE_INVALID", "No page size was set for this download. Enter a page size of 1 or more in the plugin settings.");
    };
    let state = json!({"configuratorPage":true,"records":records,"inventory":inventory,"prepare":r.state["_hostPreparePage"],"company":r.company,"size":size});
    advance(
        state,
        crate::configurator_bulk::start(&r.company, &roots, size),
    )
}

pub fn resume(mut r: GuestRequest) -> GuestReply {
    if r.operation == "materialize" {
        return finish(r.state);
    }
    if let Some(error) = r.error.take() {
        return recover(r.state, error);
    }
    if r.state["appBulk"].is_object() {
        let bulk = r.state["appBulk"].take();
        let result =
            crate::configurator_app_bulk::receive(bulk, r.response.take().unwrap_or(Value::Null));
        return advance_apps(r.state, result);
    }
    let bulk = r.state["bulk"].take();
    let result = crate::configurator_bulk::receive(bulk, r.response.take().unwrap_or(Value::Null));
    advance(r.state, result)
}

fn recover(mut state: Value, diagnostic: Diagnostic) -> GuestReply {
    // Preserve visible identities and attachments. Their existing acquisition
    // path can still retain native exports if a stored read was unavailable.
    let Ok(mut inventory) = serde_json::from_value::<GuestReply>(state["inventory"].take()) else {
        return GuestReply::Failure { diagnostic };
    };
    if let GuestReply::Inventory {
        authoritative,
        diagnostics,
        ..
    } = &mut inventory
    {
        *authoritative = false;
        diagnostics.push(diagnostic);
    }
    if state["stored"].is_object() {
        state["inventory"] = json!(inventory);
        state.as_object_mut().unwrap().remove("appBulk");
        return prepare(state);
    }
    let Ok(records) = serde_json::from_value(state["records"].take()) else {
        return crate::failure("CONFIGURATOR_PAGE_INVALID", "Epicor's response contained no configurator records where some were expected. Nothing was saved for this page.");
    };
    GuestReply::InventoryData {
        records,
        inventory: Box::new(inventory),
    }
}

fn advance(
    mut state: Value,
    step: Result<crate::configurator_bulk::Step, Diagnostic>,
) -> GuestReply {
    match step {
        Err(error) => recover(state, error),
        Ok(crate::configurator_bulk::Step::Read {
            action,
            state: bulk,
        }) => {
            state["bulk"] = bulk;
            GuestReply::Action { action, state }
        }
        Ok(crate::configurator_bulk::Step::Ready { records }) => {
            state.as_object_mut().unwrap().remove("bulk");
            state["stored"] = json!(records);
            let roots: Vec<_> = records.values().map(|r| r["PcStatus"][0].clone()).collect();
            let result = crate::configurator_app_bulk::start(
                state["company"].as_str().unwrap_or(""),
                &roots,
                state["size"].as_u64().unwrap_or(0) as u32,
            );
            advance_apps(state, result)
        }
    }
}
fn advance_apps(
    mut state: Value,
    step: Result<crate::configurator_app_bulk::Step, Diagnostic>,
) -> GuestReply {
    match step {
        Err(error) => recover(state, error),
        Ok(crate::configurator_app_bulk::Step::Read {
            action,
            state: bulk,
        }) => {
            state["appBulk"] = bulk;
            GuestReply::Action { action, state }
        }
        Ok(crate::configurator_app_bulk::Step::Ready { apps }) => {
            state.as_object_mut().unwrap().remove("appBulk");
            state["apps"] = json!(apps);
            prepare(state)
        }
    }
}
fn prepare(mut state: Value) -> GuestReply {
    if state["prepare"] == true {
        let inventory: GuestReply = match serde_json::from_value(state["inventory"].clone()) {
            Ok(v) => v,
            Err(_) => {
                return crate::failure("CONFIGURATOR_PAGE_INVALID", "Herbert could not continue this download from its saved progress (saved configurator list). Start the download again.");
            }
        };
        let GuestReply::Inventory { next_cursor, .. } = inventory else {
            return crate::failure("CONFIGURATOR_PAGE_INVALID", "Invalid saved inventory kind.");
        };
        state["_progress"] =
            json!({"pageRecords":state["stored"].as_object().map_or(0,|r|r.len())});
        GuestReply::PreparedPage {
            state,
            next_cursor,
            next_state: json!({}),
        }
    } else {
        finish(state)
    }
}
fn finish(mut state: Value) -> GuestReply {
    let Ok(mut records) = serde_json::from_value::<Vec<(Identity, Value)>>(state["records"].take())
    else {
        return crate::failure("CONFIGURATOR_PAGE_INVALID", "Herbert could not continue this download from its saved progress (prepared configurator records). Start the download again.");
    };
    for (identity, data) in &mut records {
        let Some(id) = identity.keys.get("ConfigID") else {
            return crate::failure(
                "CONFIGURATOR_PAGE_INVALID",
                "Epicor returned a configurator without its ID, so it was not saved.",
            );
        };
        let Some(stored) = state["stored"].get(id).filter(|v| v.is_object()) else {
            return crate::failure(
                "CONFIGURATOR_PAGE_INVALID",
                "Missing stored configurator record set.",
            );
        };
        data["storedConfigurator"] = stored.clone();
        if let Some(app) = state["apps"].get(id) {
            data["storedApp"] = app.clone();
        }
    }
    match serde_json::from_value::<GuestReply>(state["inventory"].take()) {
        Ok(inventory) => GuestReply::InventoryData {
            records,
            inventory: Box::new(inventory),
        },
        Err(_) => crate::failure("CONFIGURATOR_PAGE_INVALID", "Herbert could not continue this download from its saved progress (configurator list). Start the download again."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "configurators".into(),
            company: "TESTCO".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({"_hostPreparePage":true}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn supplied_public_page_to_local_xml_replay() {
        let Ok(base) = std::env::var("HERBERT_CONFIGURATORS_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let read = |name: &str| -> Value {
                serde_json::from_slice(&std::fs::read(folder.join(name)).unwrap()).unwrap()
            };
            let mut visible =
                read("bulk-two-root-complete/calls/001-GetRows.json")["value"].clone();
            // A controlled attachment marker proves the native inventory survives the
            // stored-row replacement. Native attachment bytes remain separate work.
            visible["returnObj"]["PcStatusAttch"] = json!([{"Company":"TESTCO","ConfigID":"H19C1024A","DrawingSeq":1,"FileName":"owned-marker.txt","Description":"retained Ω"}]);
            let mut dictionary = read("dictionary/fields.json");
            dictionary.as_object_mut().unwrap().extend(
                read("bulk-supplement-dictionary/fields.json")
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            let rows = read("bulk-two-root-complete/rows.json");
            let mut r = request();
            let mut reads = 0;
            let records = loop {
                match crate::collectors::handle(r.clone()) {
                    GuestReply::Action { action, state } => {
                        r.state = state;
                        r.state["_hostPreparePage"] = json!(true);
                        r.response = Some(match action {
                            HostAction::NativeRead { method, .. } if method == "GetRows" => {
                                visible.clone()
                            }
                            HostAction::SharedNativeRead {
                                method, arguments, ..
                            } if method == "GetFieldList" => dictionary
                                [arguments["dataTableID"].as_str().unwrap()]["value"]
                                .clone(),
                            HostAction::NativeRead {
                                method, arguments, ..
                            } if method == "Execute" => {
                                reads += 1;
                                json!({"returnObj":{"Results":rows[arguments["queryDS"]["QueryTable"][0]["DBTableName"].as_str().unwrap()]}})
                            }
                            other => panic!("Unexpected action {other:?}"),
                        });
                    }
                    GuestReply::PreparedPage {
                        state, next_cursor, ..
                    } => {
                        // A failed app read must not discard independently acquired graphs.
                        let recovered = recover(
                            state["inner"].clone(),
                            Diagnostic::new("TEST_APP_FAILURE", "App read failed"),
                        );
                        let GuestReply::PreparedPage {
                            state: recovered, ..
                        } = recovered
                        else {
                            panic!(
                                "Stored graphs lost on app failure: {recovered:?}; keys {:?}",
                                state.as_object().unwrap().keys().collect::<Vec<_>>()
                            )
                        };
                        let GuestReply::InventoryData {
                            records: retained,
                            inventory,
                        } = finish(recovered)
                        else {
                            panic!()
                        };
                        assert!(
                            retained
                                .iter()
                                .all(|(_, d)| d["storedConfigurator"].is_object())
                        );
                        assert!(matches!(
                            *inventory,
                            GuestReply::Inventory {
                                authoritative: false,
                                ..
                            }
                        ));
                        let cursor: Value = serde_json::from_str(&next_cursor.unwrap()).unwrap();
                        assert_eq!(cursor["source"], "native");
                        r.operation = "materialize".into();
                        r.state = state;
                        r.response = None;
                    }
                    GuestReply::InventoryData { records, inventory } => {
                        assert!(matches!(
                            *inventory,
                            GuestReply::Inventory {
                                authoritative: true,
                                ..
                            }
                        ));
                        break records;
                    }
                    other => panic!("Unexpected reply {other:?}"),
                }
            };
            assert_eq!(reads, 33);
            assert_eq!(records.len(), 2);
            for (identity, inventory) in records {
                let expected = inventory["storedConfigurator"].clone();
                if identity.keys["ConfigID"] == "H19C1024A" {
                    assert_eq!(
                        inventory["response"]["returnObj"]["PcStatusAttch"][0]["Description"],
                        "retained Ω"
                    );
                }
                let attachments = inventory["response"]["returnObj"]["PcStatusAttch"].clone();
                let mut r = request();
                r.operation = "acquire".into();
                r.identity = Some(identity);
                r.state = json!({"_inventory":inventory});
                // The local route no longer reads layered images (only the native-export fallback does).
                let GuestReply::Action {
                    action: HostAction::SharedNativeRead { method, .. },
                    state,
                } = crate::collectors::handle(r.clone())
                else {
                    panic!("Missing signature capability check")
                };
                assert_eq!(method, "GetLicensedModuleAccess");
                r.state = state;
                r.response = Some(
                    read("signature-capability/001-GetLicensedModuleAccess.json")["value"].clone(),
                );
                let mut licensed = r.clone();
                licensed.response = Some(
                    json!({"returnObj":{"ModuleAccess":[{"Name":"MultiTenant","Identifier":"00000003-38f3-43c1-b65c-b97745a1a212","isLicensed":true}]}}),
                );
                assert!(matches!(
                    crate::collectors::handle(licensed),
                    GuestReply::Action {
                        action: HostAction::NativeFileExport { .. },
                        ..
                    }
                ));
                let mut unknown = r.clone();
                unknown.response = Some(json!({"returnObj":{"ModuleAccess":[]}}));
                assert!(
                    matches!(crate::collectors::handle(unknown),GuestReply::Definition {completeness:Completeness::Partial,diagnostics,..} if diagnostics.iter().any(|d|d.code=="CONFIGURATOR_SIGNATURE_UNVERIFIED"))
                );
                let GuestReply::Definition {
                    definition,
                    diagnostics,
                    completeness,
                    ..
                } = crate::collectors::handle(r)
                else {
                    panic!("Local graph requested another server operation")
                };
                assert_eq!(completeness, Completeness::Collected);
                assert!(diagnostics.is_empty());
                assert_eq!(definition["supplements"]["StoredConfigurator"], expected);
                assert_eq!(definition["definition"]["PcStatusAttch"], attachments);
                let xml =
                    crate::configurator_archive::xml(&expected, &std::collections::BTreeMap::new())
                        .unwrap();
                assert!(
                    definition["herbertSources"]["files"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|f| f["text"] == xml)
                );
            }
        }
    }
}

// Native importer skips signing checks when this module is unlicensed.
// Licensed environments retain native export so the backup does not rely on
// an exporting security manager's personal import exemption.
pub fn multi_tenant(response: &Value) -> Option<bool> {
    let rows = response.pointer("/returnObj/ModuleAccess")?.as_array()?;
    let matches: Vec<_> = rows
        .iter()
        .filter(|r| {
            r["Name"] == "MultiTenant" || r["Identifier"] == "00000003-38f3-43c1-b65c-b97745a1a212"
        })
        .collect();
    if matches.len() != 1
        || matches[0]["Name"] != "MultiTenant"
        || matches[0]["Identifier"] != "00000003-38f3-43c1-b65c-b97745a1a212"
    {
        return None;
    }
    matches[0]["isLicensed"].as_bool()
}

#[cfg(test)]
mod signature_tests {
    use super::*;
    #[test]
    fn absence_duplicates_and_wrong_types_do_not_establish_no_signature_requirement() {
        let row = json!({"Name":"MultiTenant","Identifier":"00000003-38f3-43c1-b65c-b97745a1a212","isLicensed":false});
        assert_eq!(
            multi_tenant(&json!({"returnObj":{"ModuleAccess":[row]}})),
            Some(false)
        );
        assert_eq!(
            multi_tenant(&json!({"returnObj":{"ModuleAccess":[row,row]}})),
            None
        );
        let mut bad = row.clone();
        bad["isLicensed"] = json!("false");
        assert_eq!(
            multi_tenant(&json!({"returnObj":{"ModuleAccess":[bad]}})),
            None
        );
        bad = row;
        bad["Identifier"] = json!("other");
        assert_eq!(
            multi_tenant(&json!({"returnObj":{"ModuleAccess":[bad]}})),
            None
        );
        assert_eq!(multi_tenant(&json!({})), None);
    }
}
