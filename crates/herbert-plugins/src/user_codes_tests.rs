//! Download behavior: one complete batch, then local processing.
use super::*;
fn request() -> GuestRequest {
    GuestRequest {
        abi: PLUGIN_ABI,
        operation: "discover".into(),
        category: "C01".into(),
        company: "C".into(),
        identity: None,
        cursor: None,
        page_size: Some(200),
        state: Value::Null,
        response: None,
        error: None,
    }
}
fn batch() -> Value {
    json!({"returnObj":{
    "UDCodeType":[{"Company":"C","CodeTypeID":"A","SystemFlag":false},{"Company":"C","CodeTypeID":"B","SystemFlag":false}],
    "UDCodes":[{"Company":"C","CodeTypeID":"A","CodeID":"1","CodeDesc":"Ω unchanged"},{"Company":"C","CodeTypeID":"B","CodeID":"2","CodeDesc":"password=value"}]
},"parameters":{"morePages":false}})
}
#[test]
fn one_batch_downloads_multiple_types_and_preserves_values() {
    let mut r = request();
    let GuestReply::Action {
        action: HostAction::NativeRead {
            method, arguments, ..
        },
        ..
    } = user_codes(r.clone())
    else {
        panic!()
    };
    assert_eq!(method, "GetRows");
    assert_eq!(arguments["pageSize"], 200);
    assert_eq!(arguments["whereClauseUDCodeType"], "SystemFlag = false");
    assert_eq!(arguments["whereClauseUDCodes"], "SystemFlag = false");
    r.response = Some(batch());
    let GuestReply::InventoryData { records, inventory } = user_codes(r.clone()) else {
        panic!()
    };
    assert_eq!(records.len(), 2);
    assert!(matches!(
        *inventory,
        GuestReply::Inventory {
            authoritative: true,
            next_cursor: None,
            ..
        }
    ));
    for (id, data) in records {
        assert_eq!(data["UDCodeType"][0]["CodeTypeID"], id.keys["CodeTypeID"]);
        assert_eq!(data["UDCodes"].as_array().unwrap().len(), 1);
        assert!(data.get("response").is_none(), "Return final JSON, not an acquisition cache.");
    }
}
#[test]
fn paging_continues_only_when_server_reports_more() {
    let mut r = request();
    r.response = Some(batch());
    r.response.as_mut().unwrap()["parameters"]["morePages"] = json!(true);
    let GuestReply::InventoryData { inventory, .. } = user_codes(r.clone()) else {
        panic!()
    };
    assert!(
        matches!(*inventory,GuestReply::Inventory{next_cursor:Some(ref cursor),..} if cursor=="2")
    );
    r.response.as_mut().unwrap()["parameters"] = json!({});
    assert!(matches!(user_codes(r), GuestReply::Failure { .. }));
}
#[test]
fn system_types_and_entries_are_excluded() {
    let mut r = request();
    let mut value = batch();
    value["returnObj"]["UDCodeType"]
        .as_array_mut()
        .unwrap()
        .push(json!({"Company":"C","CodeTypeID":"Stock","SystemFlag":true}));
    value["returnObj"]["UDCodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"Company":"C","CodeTypeID":"A","CodeID":"Stock","SystemFlag":true}));
    r.response = Some(value);
    let GuestReply::InventoryData { records, .. } = user_codes(r) else {
        panic!()
    };
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|(_, data)| {
        data["UDCodes"]
            .as_array()
            .unwrap()
            .len()
            == 1
    }));
}
#[test]
fn missing_child_table_and_foreign_child_do_not_claim_success() {
    let mut r = request();
    let mut value = batch();
    value["returnObj"]
        .as_object_mut()
        .unwrap()
        .remove("UDCodes");
    r.response = Some(value);
    assert!(matches!(user_codes(r.clone()), GuestReply::Failure { .. }));
    let mut value = batch();
    value["returnObj"]["UDCodes"][0]["CodeTypeID"] = json!("Other");
    r.response = Some(value);
    assert!(matches!(user_codes(r), GuestReply::Failure { .. }));
}
#[test]
fn failed_batch_does_not_start_comparison_reads() {
    let mut r = request();
    r.error = Some(Diagnostic::new("ACCESS_DENIED", "Test refusal."));
    assert!(
        matches!(user_codes(r),GuestReply::Failure{diagnostic} if diagnostic.code=="ACCESS_DENIED")
    );
}

#[test]
fn two_pages_need_two_reads_and_no_per_type_acquisition() {
    let mut r = request();
    let mut reads = 0;
    let mut definitions = Vec::new();
    for page in 1..=2 {
        let GuestReply::Action {
            action: HostAction::NativeRead { service, method, arguments, .. }, state,
        } = user_codes(r.clone()) else { panic!("Expected exactly one page read"); };
        reads += 1;
        assert_eq!(service, "Ice.BO.UserCodesSvc");
        assert_eq!(method, "GetRows");
        assert_eq!(arguments["absolutePage"], page);
        let mut response = batch();
        response["parameters"]["morePages"] = json!(page == 1);
        for row in response["returnObj"]["UDCodeType"].as_array_mut().unwrap() {
            row["CodeTypeID"] = json!(format!("{}-{page}", row["CodeTypeID"].as_str().unwrap()));
        }
        for row in response["returnObj"]["UDCodes"].as_array_mut().unwrap() {
            row["CodeTypeID"] = json!(format!("{}-{page}", row["CodeTypeID"].as_str().unwrap()));
        }
        r.state = state;
        r.response = Some(response);
        let GuestReply::InventoryData { records, inventory } = user_codes(r.clone()) else {
            panic!("The page must provide final definitions directly");
        };
        definitions.extend(records);
        let GuestReply::Inventory { next_cursor, .. } = *inventory else { panic!() };
        assert_eq!(next_cursor, if page == 1 { Some("2".into()) } else { None });
        r.cursor = next_cursor;
        r.response = None;
    }
    assert_eq!(reads, 2);
    assert_eq!(definitions.len(), 4);
    assert!(definitions.iter().all(|(_, value)| value["UDCodes"].as_array().unwrap().len() == 1));
}

fn prefixed(prefix: Value) -> GuestRequest {
    let mut r = request();
    r.state = json!({"_pluginSettings":{"codeTypeIdPrefix":prefix}});
    r
}

#[test]
fn literal_prefix_selects_complete_roots_without_normalizing_native_values() {
    let keys = ["A", "Alpha", "alpha", " A ", "A%_'", "A_", "Ωé", "Ωe\u{301}"];
    let roots: Vec<_> = keys.iter().map(|key| json!({"Company":"C","CodeTypeID":key,"Extra":"native"})).collect();
    let children: Vec<_> = keys.iter().flat_map(|key| ["one", "two"].map(|code| json!({
        "Company":"C","CodeTypeID":key,"CodeID":code,"Extra":"Ω original"
    }))).collect();
    let source = json!({"returnObj":{"UDCodeType":roots,"UDCodes":children},"parameters":{"morePages":false}});
    for (prefix, expected) in [
        ("", keys.to_vec()), ("A", vec!["A", "Alpha", "A%_'", "A_"]),
        ("a", vec!["alpha"]), (" A ", vec![" A "]), ("A%_'", vec!["A%_'"]),
        ("A_", vec!["A_"]), ("Ωé", vec!["Ωé"]), ("Ωe\u{301}", vec!["Ωe\u{301}"]),
        ("missing", vec![]), (" ", vec![" A "]),
    ] {
        let mut r = prefixed(json!(prefix));
        r.response = Some(source.clone());
        let GuestReply::InventoryData { records, inventory } = crate::handle("C01", r) else { panic!("{prefix:?}") };
        let GuestReply::Inventory { identities, authoritative, next_cursor, diagnostics } = *inventory else { panic!() };
        assert!(authoritative);
        assert!(next_cursor.is_none());
        assert!(diagnostics.is_empty(), "A valid empty selection is not an error");
        assert_eq!(identities.iter().map(|id| id.keys["CodeTypeID"].as_str()).collect::<Vec<_>>(), expected);
        assert_eq!(records.len(), expected.len());
        for (id, definition) in records {
            let key = id.keys["CodeTypeID"].as_str();
            let original_root = roots.iter().find(|row| row["CodeTypeID"] == key).unwrap();
            let original_children: Vec<_> = children.iter().filter(|row| row["CodeTypeID"] == key).collect();
            assert_eq!(definition, json!({"UDCodeType":[original_root],"UDCodes":original_children}));
        }
    }
}

#[test]
fn prefix_validates_unselected_graphs_and_keeps_empty_page_native_continuation() {
    let mut r = prefixed(json!("B"));
    let mut page = batch();
    // Valid A children must not become orphans merely because B is selected.
    r.response = Some(page.clone());
    let GuestReply::InventoryData { records, .. } = crate::handle("C01", r.clone()) else { panic!() };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0.keys["CodeTypeID"], "B");
    for field in ["Company", "CodeID", "CodeTypeID"] {
        let mut invalid = page.clone();
        invalid["returnObj"]["UDCodes"][0].as_object_mut().unwrap().remove(field);
        r.response = Some(invalid);
        assert!(matches!(crate::handle("C01", r.clone()), GuestReply::Failure { .. }), "unselected A {field}");
    }
    page["returnObj"]["UDCodeType"].as_array_mut().unwrap().pop();
    page["returnObj"]["UDCodes"].as_array_mut().unwrap().pop();
    page["parameters"]["morePages"] = json!(true);
    r.response = Some(page);
    let GuestReply::InventoryData { records, inventory } = crate::handle("C01", r.clone()) else { panic!() };
    assert!(records.is_empty());
    let GuestReply::Inventory { identities, next_cursor, authoritative, .. } = *inventory else { panic!() };
    assert!(identities.is_empty());
    assert!(authoritative);
    assert_eq!(next_cursor.as_deref(), Some("2"));
    r.cursor = next_cursor;
    r.response = None;
    let GuestReply::Action { action: HostAction::NativeRead { arguments, .. }, .. } = crate::handle("C01", r.clone()) else { panic!() };
    assert_eq!(arguments["absolutePage"], 2);
    assert_eq!(arguments["whereClauseUDCodeType"], "SystemFlag = false");
    r.response = Some(batch());
    let GuestReply::InventoryData { records, inventory } = crate::handle("C01", r) else { panic!() };
    assert_eq!(records[0].0.keys["CodeTypeID"], "B");
    assert!(matches!(*inventory, GuestReply::Inventory { next_cursor: None, .. }));
}

#[test]
fn prefix_settings_reject_non_strings_and_missing_frozen_selection_stays_unfiltered() {
    for invalid in [Value::Null, json!(false), json!(12), json!([]), json!({})] {
        let r = prefixed(invalid);
        assert!(matches!(crate::handle("C01", r), GuestReply::Failure { diagnostic } if diagnostic.code == "INVALID_SETTING"));
    }
    for state in [Value::Null, json!({"_pluginSettings":{}}),
        json!({"_pluginSettings":{"itemsPerPage":200},"_pluginState":{"config":{"codeTypeIdPrefix":"B"}}})] {
        let mut r = request();
        r.state = state;
        r.response = Some(batch());
        let GuestReply::InventoryData { records, .. } = crate::handle("C01", r) else { panic!() };
        assert_eq!(records.len(), 2, "Missing frozen selection cannot use current saved filter");
    }
}

#[test]
fn prefix_saved_default_blank_override_and_exact_acquisition_are_independent() {
    let mut r = request();
    r.operation = "descriptor".into();
    r.state = json!({"_pluginState":{"config":{"codeTypeIdPrefix":" B "}}});
    let GuestReply::Descriptor { category, .. } = crate::handle("C01", r) else { panic!() };
    let saved = resolve_plugin_settings(&category.settings, &Value::Null).unwrap();
    assert_eq!(saved["codeTypeIdPrefix"], " B ");
    let cleared = resolve_plugin_settings(&category.settings, &json!({"codeTypeIdPrefix":""})).unwrap();
    assert_eq!(cleared["codeTypeIdPrefix"], "");
    assert_eq!(saved["codeTypeIdPrefix"], " B ", "Run overrides do not save");
    let mut r = request();
    r.operation = "configure".into();
    r.state = json!({"_pluginState":{"config":saved},"values":{"codeTypeIdPrefix":""},"selected":false});
    let GuestReply::State { plugin_state, .. } = crate::handle("C01", r) else { panic!() };
    assert_eq!(plugin_state["config"]["codeTypeIdPrefix"], "", "Saving blank clears the saved filter");
    assert_eq!(plugin_state["selected"], false);

    let mut r = prefixed(json!("B"));
    r.operation = "acquire".into();
    r.identity = Some(identity("C01", "UDCodeType", "C", "CodeTypeID", "A"));
    let GuestReply::Action { action: HostAction::NativeRead { arguments, .. }, .. } = crate::handle("C01", r.clone()) else { panic!() };
    assert_eq!(arguments["whereClauseUDCodeType"], "CodeTypeID = 'A' AND SystemFlag = false");
    let mut source = batch();
    source["returnObj"]["UDCodeType"].as_array_mut().unwrap().pop();
    source["returnObj"]["UDCodes"].as_array_mut().unwrap().pop();
    r.response = Some(source.clone());
    let GuestReply::Definition { identity, definition, .. } = crate::handle("C01", r) else { panic!() };
    assert_eq!(identity.keys["CodeTypeID"], "A");
    assert_eq!(definition, source["returnObj"]);
}
