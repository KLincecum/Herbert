//! Opt-in lab reads for local BAQ export qualification. No helper is installed.
use crate::input::AuthMode;
use crate::{connection::Connection, profiles, scheduler::Budget};
use herbert_contract::OperatingSettings;
use serde_json::json;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Requires the isolated C04 WASM candidate and a retained lab response."]
async fn local_baq_large_response_wasm() {
    use herbert_contract::{GuestReply, GuestRequest, HostAction, PLUGIN_ABI};
    let base = PathBuf::from(std::env::var("HERBERT_BAQ_EVIDENCE").unwrap());
    let plugin = crate::plugins::Plugin::load(&base.join("candidate/plugins/BAQs/BAQs.wasm"))
        .await
        .unwrap();
    let original: serde_json::Value = serde_json::from_slice(
        &std::fs::read(base.join("lab-2026-1-complete/rows-100-1.json")).unwrap(),
    )
    .unwrap();
    let mut data = original["returnObj"].clone();
    // Repeat a real definition and enlarge a text field. This is an offline
    // transport and packaging test. It does not establish native import proof.
    for rows in data.as_object_mut().unwrap().values_mut() {
        let source = rows.as_array().unwrap().clone();
        let mut copies = Vec::new();
        for index in 0..24 {
            for row in &source {
                let mut row = row.clone();
                row["QueryID"] = json!(format!("OfflineVolume{index}"));
                row["SysRowID"] = json!(format!("00000000-0000-0000-0000-{index:012}"));
                copies.push(row);
            }
        }
        *rows = json!(copies);
    }
    for root in data["DynamicQueryDesigner"].as_array_mut().unwrap() {
        root["Description"] = json!("x".repeat(700_000));
    }
    let response = json!({"returnObj":data,"parameters":{"morePages":false}});
    let input_bytes = serde_json::to_vec(&response).unwrap().len();
    assert!(input_bytes > 16_000_000);
    let mut request = GuestRequest {
        abi: PLUGIN_ABI,
        operation: "discover".into(),
        category: "C04".into(),
        company: "TESTCO".into(),
        identity: None,
        cursor: None,
        page_size: Some(100),
        state: json!({"phase":"rows","version":"5.2.100.0","page":1}),
        response: Some(response),
        error: None,
    };
    let started = std::time::Instant::now();
    let GuestReply::Action {
        action: HostAction::Query { .. },
        state,
    } = plugin
        .call(request.clone(), CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("The page did not request its diagrams.")
    };
    request.state = state;
    request.response = Some(json!({"rows":[]}));
    let GuestReply::InventoryData { records, inventory } = plugin
        .call(request, CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("The plugin did not return the completed page.")
    };
    assert_eq!(records.len(), 24);
    assert!(matches!(
        *inventory,
        GuestReply::Inventory {
            authoritative: true,
            ..
        }
    ));
    let receipt = json!({"proof":"synthetic offline compiled WASM",
        "inputBytes":input_bytes,"files":records.len(),"elapsedMs":started.elapsed().as_millis()});
    std::fs::write(
        base.join("wasm-volume.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    println!("{receipt}");
}

#[tokio::test]
#[ignore = "Requires an explicit lab profile and output directory."]
async fn gather_local_baq_evidence() {
    let profile = std::env::var("HERBERT_BAQ_LAB_PROFILE").unwrap();
    let endpoint = match profile.as_str() {
        "lab-2026-1" => "https://kinetic-2026-1.lab.test:9448/",
        "lab-2025-2" => "https://kinetic-2025-2.lab.test:9449/",
        _ => panic!("Select one of the two lab profiles."),
    };
    let output = PathBuf::from(std::env::var("HERBERT_BAQ_LAB_OUTPUT").unwrap());
    assert!(!output.exists(), "Preserve previous evidence.");
    std::fs::create_dir_all(&output).unwrap();
    let credentials = profiles::secure_read(endpoint, &profile).unwrap();
    let mode = if credentials.api_key.as_ref().is_some_and(|v| !v.is_empty()) {
        AuthMode::BasicApiKey
    } else {
        AuthMode::Session
    };
    let connection = Connection::new(
        crate::input::endpoint(endpoint).unwrap(),
        "TESTCO".into(),
        mode,
        credentials,
        OperatingSettings::default(),
        Budget::new(4).unwrap(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    connection.connect(&cancel).await.unwrap();
    use futures_util::FutureExt;
    let result = std::panic::AssertUnwindSafe(gather(&connection, &output, &cancel))
        .catch_unwind()
        .await;
    connection.logout(&cancel).await.unwrap();
    match result {
        Ok(result) => result.unwrap(),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

async fn gather(
    connection: &Connection,
    output: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<(), herbert_contract::Diagnostic> {
    let tables = [
        "DynamicQueryDesigner",
        "QueryCtrlDesigner",
        "QueryCtrlValuesDesigner",
        "QueryCustomActionDesigner",
        "QueryExecuteSettingDesigner",
        "QueryParameterDesigner",
        "QueryReferenceDesigner",
        "QueryParameterBindingDesigner",
        "QuerySubQueryDesigner",
        "QueryRelationDesigner",
        "QueryRelationFieldDesigner",
        "QuerySortByDesigner",
        "QueryWhereItemDesigner",
        "QueryGroupByDesigner",
        "QueryTableDesigner",
        "QueryFieldDesigner",
        "QueryFieldAttributeDesigner",
        "QueryFunctionCallDesigner",
        "QueryUpdateFieldDesigner",
        "QueryUpdateSettingsDesigner",
        "QueryValueSetItemsDesigner",
    ];
    let service = "Ice.BO.BAQDesignerSvc";
    let mut receipts = Vec::new();
    let bulk_probe = std::env::var("HERBERT_BAQ_BULK_PROBE").as_deref() == Ok("1");
    let mut roots = Vec::new();
    // These are test points, not supported maximum page sizes.
    let sizes: Vec<i32> = if bulk_probe {
        std::env::var("HERBERT_BAQ_PROBE_SIZES")
            .unwrap_or_else(|_| "24,100,500".into())
            .split(',')
            .map(|v| v.parse::<i32>().unwrap())
            .collect()
    } else {
        vec![24, 100, 500, 1000, 2000]
    };
    assert!(sizes.iter().all(|size| *size > 0));
    for size in &sizes {
        let mut page = 1;
        loop {
            let mut arguments = json!({"pageSize":size,"absolutePage":page});
            for table in tables {
                arguments[format!("whereClause{table}")] = json!("");
            }
            arguments["whereClauseDynamicQueryDesigner"] = json!(
                "(Company = 'TESTCO' OR Company = '') AND SystemFlag = false AND Updatable = false"
            );
            if bulk_probe {
                // Vendor queries supply a read-only volume specimen in the labs.
                // This qualification branch does not export or collect those queries.
                arguments["whereClauseDynamicQueryDesigner"] = json!("Updatable = false");
            }
            let response = connection
                .native_read(service, "GetRows", arguments, false, cancel)
                .await?;
            let name = format!("rows-{size}-{page}.json");
            if !bulk_probe {
                std::fs::write(output.join(&name), &response.raw).unwrap();
            }
            let counts: serde_json::Map<String, serde_json::Value> = response.value["returnObj"]
                .as_object()
                .unwrap()
                .iter()
                .filter_map(|(name, rows)| {
                    rows.as_array()
                        .map(|rows| (name.clone(), json!(rows.len())))
                })
                .collect();
            receipts.push(json!({"pageSize":size,"page":page,"file":if bulk_probe {None} else {Some(name)},"bytes":response.raw.len(),"elapsedMs":response.elapsed_ms,"rowCounts":counts}));
            std::fs::write(
                output.join("receipts.json"),
                serde_json::to_vec_pretty(
                    &json!({"endpoint":connection.endpoint,"receipts":receipts}),
                )
                .unwrap(),
            )
            .unwrap();
            if bulk_probe {
                if std::env::var("HERBERT_BAQ_RECONCILE").as_deref() == Ok("1") {
                    let data = &response.value["returnObj"];
                    let headers = data["DynamicQueryDesigner"].as_array().unwrap();
                    let owns = |row: &serde_json::Value, root: &serde_json::Value| {
                        row["Company"] == root["Company"] && row["QueryID"] == root["QueryID"]
                    };
                    let mut checked_rows = 0;
                    for rows in data.as_object().unwrap().values() {
                        for row in rows.as_array().unwrap() {
                            assert!(
                                headers.iter().any(|root| owns(row, root)),
                                "A bulk row has no parent."
                            );
                            checked_rows += 1;
                        }
                    }
                    // Compare boundary queries with independent complete reads.
                    // Keep values in memory; retain only counts and the verdict.
                    let mut compared = Vec::new();
                    for root in [headers.first().unwrap(), headers.last().unwrap()] {
                        let mut args = json!({"pageSize":1,"absolutePage":1});
                        for table in tables {
                            args[format!("whereClause{table}")] = json!("");
                        }
                        args["whereClauseDynamicQueryDesigner"] = json!(format!(
                            "Company = '{}' AND QueryID = '{}'",
                            root["Company"].as_str().unwrap().replace('\'', "''"),
                            root["QueryID"].as_str().unwrap().replace('\'', "''")
                        ));
                        let single = connection
                            .native_read(service, "GetRows", args, false, cancel)
                            .await?;
                        for (table, rows) in data.as_object().unwrap() {
                            let normalized = |row: &serde_json::Value| {
                                let mut row = row.clone();
                                // Epicor allocates new IDs for inferred attributes on each read.
                                // Native export omits these rows; compare their other values here.
                                if table == "QueryFieldAttributeDesigner"
                                    && row["IsOverriden"] == false
                                {
                                    row["SysRowID"] = json!("");
                                }
                                row.to_string()
                            };
                            let mut bulk: Vec<String> = rows
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter(|row| owns(row, root))
                                .map(normalized)
                                .collect();
                            let mut individual: Vec<String> = single.value["returnObj"][table]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(normalized)
                                .collect();
                            bulk.sort();
                            individual.sort();
                            assert_eq!(
                                bulk, individual,
                                "The bulk and individual {table} rows differ."
                            );
                        }
                        compared.push(root["QueryID"].clone());
                    }
                    std::fs::write(
                        output.join(format!("reconciliation-{size}.json")),
                        serde_json::to_vec_pretty(
                            &json!({"headers":headers.len(),"checkedRows":checked_rows,
                            "independentQueries":compared,"allValuesEqual":true}),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                }
                break;
            }
            if *size == 500 {
                roots.extend(
                    response.value["returnObj"]["DynamicQueryDesigner"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .cloned(),
                );
            }
            if response.value["parameters"]["morePages"] == false {
                break;
            }
            assert_eq!(response.value["parameters"]["morePages"], true);
            page += 1;
        }
    }
    for (index, root) in roots.iter().enumerate() {
        for method in ["LoadQueryDiagram", "LoadKineticQueryDiagram", "ExportBaq"] {
            let arguments = if method == "ExportBaq" {
                json!({"queryId":root["QueryID"],"options":{}})
            } else {
                json!({"queryID":root["QueryID"],"companyID":root["Company"]})
            };
            let response = connection
                .native_read(service, method, arguments, method == "ExportBaq", cancel)
                .await?;
            let name = format!("query-{index}-{method}.json");
            std::fs::write(output.join(&name), &response.raw).unwrap();
            receipts.push(json!({"file":name,"queryID":root["QueryID"],"company":root["Company"],"bytes":response.raw.len(),"elapsedMs":response.elapsed_ms}));
        }
    }
    std::fs::write(
        output.join("receipts.json"),
        serde_json::to_vec_pretty(&json!({"endpoint":connection.endpoint,"receipts":receipts}))
            .unwrap(),
    )
    .unwrap();
    Ok(())
}
