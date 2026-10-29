use super::*;
fn selection_state(family:&str,prefix:&str)->Value {json!({"_pluginSettings":{"definitionFamily":family,"definitionIdPrefix":prefix}})}
fn form_records()->Vec<(Identity,Value)> { ["A","B"].into_iter().map(|id|(identity("BPMDataForm","","FormId",id.into()),json!({"IPForm":[{"FormId":id,"Description":"Ω original\r\n"}],"IPFormBPAction":[{"FormId":id,"SysRowID":"action"}],"IPFormCtrl":[{"FormId":id,"ControlId":"control","SysRowID":"control"}],"IPFormCtrlValues":[{"FormId":id,"ParentId":"control","SysRowID":"value","Value":"2026-01-02"}],"ExtensionTables":[]}))).collect() }
fn finish_records(reply:GuestReply)->(Vec<(Identity,Value)>,Vec<Diagnostic>){
    let GuestReply::PreparedPage {state,..}=reply else{panic!()};let items=state["_hostWorkItems"].as_array().cloned().unwrap_or_else(||vec![state.clone()]);let mut records=vec![];let mut errors=vec![];
    for mut state in items {loop{match output(state){GuestReply::ReadyRecords {records:one,state:next}=>{records.extend(one);state=next;},GuestReply::Inventory {diagnostics,..}=>{errors.extend(diagnostics);break;},other=>panic!("{other:?}")}}}(records,errors)
}
#[test]
fn family_prefix_preserves_complete_form_bytes_and_independent_output_settings() {
    let baseline=finish_records(finish_selected(form_records(),Some("raw".into()),true,false,&selection_state("all","")));
    for independent in [false,true] {let state=selection_state("forms","A");let reply=finish_selected(form_records(),Some("raw".into()),true,independent,&state);if let GuestReply::PreparedPage {state:prepared,next_state,next_cursor}= &reply {assert_eq!(prepared["_pluginSettings"],state["_pluginSettings"]);assert_eq!(next_state["_pluginSettings"],state["_pluginSettings"]);assert_eq!(next_cursor.as_deref(),Some("raw"));}let result=finish_records(reply);assert_eq!(result.0,baseline.0.iter().filter(|(id,_)|id.keys["FormId"]=="A").cloned().collect::<Vec<_>>());assert!(result.1.is_empty());assert_eq!(result.0[0].1["IPFormCtrlValues"][0]["Value"],"2026-01-02");}
    for prefix in ["a"," A","%","Ω"] {let result=finish_records(finish_selected(form_records(),Some("raw".into()),true,true,&selection_state("all",prefix)));assert!(result.0.is_empty());assert!(result.1.is_empty());}
}
#[test]
fn excluded_output_and_read_errors_remain_incomplete_with_zero_matches_and_good_peers() {
    let mut records=form_records();records[1].1["herbertNativePageExtensions"]=json!([{"secret":"EXCLUDED_PAYLOAD"}]);
    for independent in [false,true] {let (selected,errors)=finish_records(finish_selected(records.clone(),Some("raw".into()),true,independent,&selection_state("forms","A")));assert_eq!(selected.len(),1);assert!(errors.is_empty(),"a prefix-excluded record is not validated or reported: {errors:?}");assert!(!serde_json::to_string(&errors).unwrap().contains("EXCLUDED_PAYLOAD"));let (selected,errors)=finish_records(finish_selected(records.clone(),Some("raw".into()),true,independent,&selection_state("postingRules","Missing")));assert!(selected.is_empty());assert!(!errors.is_empty());}
    let mut r=request();r.state=selection_state("structuredOutput","Missing");r.state["records"]=json!(form_records());r.error=Some(Diagnostic::new("DENIED","read failed"));let result=finish_records(handle(r));assert!(result.0.is_empty());assert!(result.1.iter().all(|error|error.code=="DENIED"));assert_eq!(result.1.len(),2);
}
#[test]
fn selected_empty_native_pages_continue_and_exact_internal_discovery_ignores_controls() {
    let mut r=request();r.state=selection_state("forms","Missing");r.state["_hostPreparePage"]=json!(true);let source=form_records();let body=json!({"IPForm":source.iter().map(|(_,data)|data["IPForm"][0].clone()).collect::<Vec<_>>(),"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[]});r.response=Some(json!({"returnObj":body,"parameters":{"morePages":true}}));let GuestReply::PreparedPage {state,next_cursor,next_state}=handle(r.clone())else{panic!()};assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["page"],2);assert!(state["records"].as_array().unwrap().is_empty());assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);
    r.operation="acquire".into();r.identity=Some(source[0].0.clone());r.response=None;r.state=selection_state("postingRules","Missing");r.state["_pluginSettings"]=json!({"definitionFamily":false,"definitionIdPrefix":false});let GuestReply::Action {state,..}=handle(r.clone())else{panic!()};r.state=state;r.response=Some(json!({"returnObj":{"IPForm":[{"FormId":"A"}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[]},"parameters":{"morePages":false}}));assert!(matches!(handle(r),GuestReply::Definition {completeness:Completeness::Collected,..}));
}
#[test]
fn structured_hierarchy_and_independent_posting_graphs_preserve_whole_owner_output() {
    let mut body=json!({"RptStructuredOutputDef":[],"RptStructuredOutputFile":[],"RptStructuredOutputFileElement":[],"RptStructuredOutputFileXMLNS":[]});
    for id in ["A","B"] {body["RptStructuredOutputDef"].as_array_mut().unwrap().push(json!({"Company":"","RptStructuredOutputDefID":id,"SystemFlag":false}));for table in ["RptStructuredOutputFile","RptStructuredOutputFileXMLNS"] {body[table].as_array_mut().unwrap().push(json!({"Company":"","RptStructuredOutputDefID":id,"RptStructuredOutputFileID":"file","SysRowID":format!("{id}-{table}")}));}for (element,parent) in [(1,0),(2,1)] {body["RptStructuredOutputFileElement"].as_array_mut().unwrap().push(json!({"Company":"","RptStructuredOutputDefID":id,"RptStructuredOutputFileID":"file","ElementID":element,"ParentElementID":parent,"SysRowID":format!("{id}-{element}"),"Constant":"2026-10-27"}));}}
    let records=native_records(&body,1,"TESTCO").unwrap();let baseline=finish_records(finish_selected(records.clone(),None,true,false,&selection_state("all","")));let selected=finish_records(finish_selected(records,None,true,true,&selection_state("structuredOutput","A")));assert_eq!(selected.0,baseline.0.into_iter().filter(|(id,_)|id.keys["RptStructuredOutputDefID"]=="A").collect::<Vec<_>>());assert!(selected.1.is_empty());assert_eq!(selected.0[0].1["RptStructuredOutputFileElement"][1]["Constant"],"2026-10-27");
    body["RptStructuredOutputFileElement"][3]["ParentElementID"]=json!(2);assert!(native_records(&body,1,"TESTCO").is_err());
    let mut posting=json!({});for table in POSTING_TABLES.iter().copied().chain(["COAMap"]){posting[table]=json!([]);}posting["ACTType"]=json!([{"Company":"TESTCO","ACTTypeUID":7,"Name":"Original Ω"}]);let record=(identity("PostingRules","TESTCO","ACTTypeUID","7".into()),posting);
    let all=finish_records(finish_selected(vec![record.clone()],None,true,false,&selection_state("all","")));assert!(all.1.is_empty());for family in ["all","postingRules"]{let prefixed=finish_records(finish_selected(vec![record.clone()],None,true,true,&selection_state(family,"Unrelated")));assert_eq!(prefixed,all);}
    let excluded=finish_records(finish_selected(vec![record.clone()],None,true,true,&selection_state("forms","")));assert!(excluded.0.is_empty());assert!(excluded.1.is_empty());let mut broken=record;broken.1["ACTType"]=json!([]);let result=finish_records(finish_selected(vec![broken],None,true,true,&selection_state("forms","Missing")));assert!(result.0.is_empty());assert_eq!(result.1.len(),1);assert_eq!(result.1[0].identity.as_ref().unwrap().keys["ACTTypeUID"],"7");
}
#[test]
fn independent_work_is_negotiated_and_preserves_exact_output_and_native_cursor() {
    let records: Vec<_> = ["A","B"].into_iter().map(|name|(
        identity("BPMDataForm","","FormId",name.into()),
        json!({"IPForm":[{"FormId":name,"Description":"Unicode Ω ‘quote’\r\n"}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[],"ExtensionTables":[]})
    )).collect();
    let next = Some("native-next-page".to_owned());
    let GuestReply::PreparedPage { state: legacy, .. } =
        finish_independent(records.clone(), next.clone(), true, false)
    else {
        panic!()
    };
    assert!(legacy.get("_hostWorkItems").is_none());
    let GuestReply::PreparedPage {
        state, next_cursor, ..
    } = finish_independent(records, next.clone(), true, true)
    else {
        panic!()
    };
    assert_eq!(next_cursor, next);
    assert_eq!(state["_hostWorkNextCursor"], "native-next-page");
    assert_eq!(state["_progress"]["pageRecords"], 2);
    let mut old = legacy;
    for item in state["_hostWorkItems"].as_array().unwrap() {
        assert_eq!(item["records"].as_array().unwrap().len(), 1);
        let GuestReply::ReadyRecords {
            records: expected,
            state: remaining,
        } = output(old)
        else {
            panic!()
        };
        old = remaining;
        let GuestReply::ReadyRecords {
            records: actual,
            state: finished,
        } = output(item.clone())
        else {
            panic!()
        };
        assert_eq!(actual, expected);
        assert!(matches!(
            output(finished),
            GuestReply::Inventory {
                authoritative: true,
                next_cursor: None,
                ..
            }
        ));
    }
    assert!(
        matches!(output(old),GuestReply::Inventory{next_cursor:Some(cursor),..}if cursor=="native-next-page")
    );
}
#[test]
fn targeted_requests_bind_exact_native_keys_and_reject_foreign_scope() {
    let mut r = request();
    r.operation = "acquire".into();
    r.identity = Some(identity("BPMDataForm", "", "FormId", "A'B".into()));
    let GuestReply::Action {
        action: HostAction::NativeRead { arguments, .. },
        state,
    } = handle(r.clone())
    else {
        panic!()
    };
    assert_eq!(arguments["whereClauseIPForm"], "FormId = 'A''B'");
    r.state = state;
    r.response = Some(
        json!({"returnObj":{"IPForm":[{"FormId":"A'B"}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[],"ExtensionTables":[]},"parameters":{"morePages":false}}),
    );
    assert!(matches!(
        handle(r.clone()),
        GuestReply::Definition {
            completeness: Completeness::Collected,
            ..
        }
    ));
    r.identity = Some(identity(
        "PostingRules",
        "FOREIGN",
        "ACTTypeUID",
        "7".into(),
    ));
    assert!(matches!(handle(r), GuestReply::Failure { .. }));
}
fn request() -> GuestRequest {
    GuestRequest {
        abi: PLUGIN_ABI,
        operation: "discover".into(),
        category: "business-rules-and-mappings".into(),
        company: "TESTCO".into(),
        identity: None,
        cursor: None,
        page_size: Some(2),
        state: json!({"_hostPreparePage":true}),
        response: None,
        error: None,
    }
}
#[test]
fn native_pages_reject_missing_foreign_or_duplicate_children() {
    let p = json!({"IPForm":[{"FormId":"A"}],"IPFormBPAction":[],"IPFormCtrl":[{"FormId":"A","ControlId":"c","SysRowID":"id"}],"IPFormCtrlValues":[]});
    assert_eq!(native_records(&p, 0, "TESTCO").unwrap().len(), 1);
    let mut bad = p.clone();
    bad.as_object_mut().unwrap().remove("IPFormCtrlValues");
    assert!(native_records(&bad, 0, "TESTCO").is_err());
    let mut bad = p.clone();
    bad["IPFormCtrl"][0]["FormId"] = json!("Foreign");
    assert!(native_records(&bad, 0, "TESTCO").is_err());
    let mut bad = p.clone();
    bad["IPFormCtrl"]
        .as_array_mut()
        .unwrap()
        .push(p["IPFormCtrl"][0].clone());
    assert!(native_records(&bad, 0, "TESTCO").is_err());
    let mut r = request();
    r.response = Some(json!({"returnObj":p}));
    assert!(matches!(
        handle(r),
        GuestReply::Inventory {
            authoritative: false,
            next_cursor: Some(_),
            ..
        }
    ));
}
#[test]
fn failures_keep_received_posting_source_and_do_not_emit_recovery_files() {
    let id = identity("PostingRules", "TESTCO", "ACTTypeUID", "7".into());
    let mut r = request();
    r.cursor = Some(json!({"stream":2,"page":1}).to_string());
    r.state = json!({"phase":"rows","records":[[id,{"ACTType":[{"ACTTypeUID":7,"Company":"TESTCO"}],"ACTRevision":[{"RevisionCode":"R1"}]}]]});
    r.error = Some(Diagnostic::new("DENIED", "Native child read refused"));
    let GuestReply::PreparedPage { state, .. } = handle(r.clone()) else {
        panic!()
    };
    r.operation = "materialize".into();
    r.state = state;
    r.error = None;
    let GuestReply::ReadyRecords { records, state } = handle(r.clone()) else {
        panic!()
    };
    assert_eq!(records[0].1["ACTRevision"][0]["RevisionCode"], "R1");
    assert!(records[0].1["herbertSources"].is_null());
    assert_eq!(
        records[0].1["herbertCollectionStatus"]["completeness"],
        "partial"
    );
    r.state = state;
    assert!(
        matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..} if diagnostics[0].code=="DENIED")
    );
}
#[test]
fn descriptor_is_pure_bulk_and_excludes_side_effecting_posting_service() {
    let reads = methods();
    assert_eq!(reads.len(), 4);
    assert!(!reads.iter().any(|(s, _)| s == "Erp.BO.ACTTypeSvc"));
    let mut r = request();
    r.operation = "descriptor".into();
    let GuestReply::Descriptor { category, .. } = crate::handle("business-rules-and-mappings", r) else {
        panic!()
    };
    assert!(category.roles.iter().any(|r| r == "pagedDefinitions"));
    assert!(category.helpers.is_empty());
}
#[test]
fn supplied_native_data_survives_pages_and_every_serialized_continuation() {
    let Ok(base) = std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_REPLAY") else {
        return;
    };
    for lane in ["2026.1", "2025.2"] {
        let folder = std::path::Path::new(&base).join(lane);
        let read = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(folder.join(name)).unwrap()).unwrap()
        };
        let posting = read("posting-stored/rows.json");
        let schema: Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(&base)
                    .parent()
                    .unwrap()
                    .join("business-rules-and-mappings-output-proof")
                    .join(lane)
                    .join("graph-routes/schema.json"),
            )
            .unwrap(),
        )
        .unwrap();

        let mut bodies = vec![];
        for (stream, ids) in [
            (0, ["H20F1024A", "H20F1024B"]),
            (1, ["H20S1024A", "H20S1024B"]),
        ] {
            let list = if stream == 0 { FORMS } else { STRUCTURED };
            let mut body = json!({"ExtensionTables":[]});
            for t in list {
                body[*t] = json!([]);
            }
            for id in ids {
                let ds = read(&format!("fixtures/{id}.json"));
                for t in list {
                    body[*t]
                        .as_array_mut()
                        .unwrap()
                        .extend(ds[*t].as_array().unwrap().clone());
                }
            }
            bodies.push(body);
        }
        let mut dictionaries = BTreeMap::new();
        for (i, t) in POSTING_TABLES.iter().enumerate() {
            dictionaries.insert(
                *t,
                read(&format!(
                    "posting-stored/calls/{:03}-GetFieldList.json",
                    i * 2 + 1
                ))["value"]
                    .clone(),
            );
        }
        let mut r = request();
        let mut outputs = vec![];
        let mut pages = 0;
        loop {
            // Simulate a fresh guest process at every host boundary.
            r = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
            match crate::handle("business-rules-and-mappings", r.clone()) {
                GuestReply::Action { action, state } => {
                    r.state = state;
                    r.state["_hostPreparePage"] = json!(true);
                    r.response = Some(match action {
                        HostAction::NativeRead {
                            service,
                            method,
                            arguments,
                            ..
                        } if method == "GetRows" => {
                            let stream = if service == "Ice.BO.InfoPromptFormSvc" {
                                0
                            } else {
                                1
                            };
                            let list = if stream == 0 { FORMS } else { STRUCTURED };
                            let page = arguments["absolutePage"].as_u64().unwrap() as usize;
                            let size = arguments["pageSize"].as_u64().unwrap() as usize;
                            let roots = bodies[stream][list[0]].as_array().unwrap();
                            let selected: Vec<_> = roots
                                .iter()
                                .skip((page - 1) * size)
                                .take(size)
                                .cloned()
                                .collect();
                            let key = if stream == 0 {
                                "FormId"
                            } else {
                                "RptStructuredOutputDefID"
                            };
                            let ids: BTreeSet<_> =
                                selected.iter().map(|r| r[key].as_str().unwrap()).collect();
                            let mut body = json!({"ExtensionTables":[]});
                            for t in list {
                                body[*t] = json!(
                                    bodies[stream][*t]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .filter(|r| ids.contains(r[key].as_str().unwrap()))
                                        .cloned()
                                        .collect::<Vec<_>>()
                                );
                            }
                            json!({"returnObj":body,"parameters":{"morePages":page*size<roots.len()}})
                        }
                        HostAction::SharedNativeRead { method, .. } if method == "GetRows" => {
                            schema["value"].clone()
                        }
                        HostAction::SharedNativeRead {
                            method, arguments, ..
                        } if method == "GetFieldList" => {
                            dictionaries[arguments["dataTableID"].as_str().unwrap()].clone()
                        }
                        HostAction::NativeRead {
                            method, arguments, ..
                        } if method == "Execute" => {
                            if arguments["queryDS"]["DynamicQuery"][0]["QueryID"]
                                == "HerbertPostingGraph"
                            {
                                let mut rows = vec![];
                                for t in POSTING_TABLES.iter().copied().chain(["COAMap"]) {
                                    let stored = posting[t].as_array().cloned().unwrap_or_default();
                                    for r in &stored {
                                        rows.push(json!({"Kind":t,"RowId":r["SysRowID"],"Payload":serde_json::to_string(&schema["value"]["returnObj"]["ZDataField"].as_array().unwrap().iter().filter(|f|f["DataTableID"]==t&&f["External"]==false).map(|f|r[f["FieldName"].as_str().unwrap()].clone()).collect::<Vec<_>>()).unwrap(),"Total":-1}));
                                    }
                                    rows.push(json!({"Kind":t,"RowId":null,"Payload":null,"Total":stored.len()}));
                                }
                                json!({"returnObj":{"Results":rows}})
                            } else {
                                let table = arguments["queryDS"]["QueryTable"][0]["DBTableName"]
                                    .as_str()
                                    .unwrap();
                                assert_eq!(
                                    arguments["queryDS"]["QueryTable"][0]["DBSchemaName"],
                                    "Erp"
                                );
                                let size = arguments["queryDS"]["QuerySubQuery"][0]["OrderByFetch"]
                                    .as_str()
                                    .unwrap()
                                    .parse::<usize>()
                                    .unwrap();
                                let after = arguments["queryDS"]["QueryWhereItem"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .find(|c| c["FieldName"] == "SysRowID" && c["CompOp"] == ">")
                                    .and_then(|c| c["RValue"].as_str())
                                    .map(|s| s.trim_start_matches("N'").trim_end_matches('\''));
                                let mut rows = posting[table].as_array().unwrap().clone();
                                rows.sort_by(|a, b| {
                                    a["SysRowID"].as_str().cmp(&b["SysRowID"].as_str())
                                });
                                rows.retain(|r| {
                                    after.is_none_or(|a| r["SysRowID"].as_str().unwrap() > a)
                                });
                                rows.truncate(size);
                                json!({"returnObj":{"Results":rows}})
                            }
                        }
                        other => panic!("Unexpected native action {other:?}"),
                    });
                }
                GuestReply::PreparedPage { state, .. } => {
                    pages += 1;
                    r.operation = "materialize".into();
                    r.state = state;
                    r.response = None;
                }
                GuestReply::ReadyRecords { records, state } => {
                    for (id, data) in records {
                        assert_eq!(
                            data["herbertCollectionStatus"]["completeness"], "collected",
                            "{id:?} {data}"
                        );
                        assert!(data["herbertSources"]["files"].as_array().unwrap().len() >= 1);
                        outputs.push(id);
                    }
                    r.state = state;
                    r.response = None;
                }
                GuestReply::Inventory {
                    next_cursor,
                    authoritative,
                    diagnostics,
                    ..
                } => {
                    assert!(authoritative, "{diagnostics:?}");
                    assert!(diagnostics.is_empty());
                    if next_cursor.is_none() {
                        break;
                    }
                    r = request();
                    r.cursor = next_cursor;
                }
                other => panic!("Unexpected reply {other:?}"),
            }
        }
        assert_eq!(outputs.len(), 5);
        assert_eq!(pages, 3);
    }
}
