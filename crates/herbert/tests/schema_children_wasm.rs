//! Offline acceptance of the actual packaged C02 module through the host runtime.
use herbert_contract::*;
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Requires HERBERT_C02_WASM pointing to the built release module; no server is used"]
async fn compiled_schema_children_preserve_completed_paging() {
    let path = PathBuf::from(std::env::var("HERBERT_C02_WASM").unwrap());
    let plugin = herbert::plugins::Plugin::load(&path).await.unwrap();
    let mut request = GuestRequest {
        abi: PLUGIN_ABI, operation: "acquire".into(), category: "C02".into(),
        company: "TESTCO".into(),
        identity: Some(Identity { category: "C02".into(), kind: "UDTableSchema".into(),
            scope: Scope::Global, keys: BTreeMap::from([
                ("SystemCode".into(), "ICE".into()), ("DataTableID".into(), "UD01".into())]) }),
        cursor: None, page_size: Some(2), state: json!({}),
        response: None, error: None,
    };
    let row = |table: &str, id: &str| json!({"SystemCode":"Ice","DataTableID":table,
        "FieldName":format!("Custom_{id}_c"),"LookupID":id,"LinkID":id,"KeyID":id,"Seq":1,
        "SystemFlag":false,"SysRowID":format!("00000000-0000-0000-0000-00000000000{id}"),"Value":"Unchanged Ω"});
    let mut reads = 0;
    let mut native_reads = 0;
    loop {
        let reply = plugin.call(request.clone(), CancellationToken::new()).await.unwrap();
        match reply {
            GuestReply::Action { action: HostAction::NativeRead { service, method, .. }, state } => {
                native_reads += 1;
                assert_eq!(native_reads, 1);
                assert_eq!(service, "Ice.BO.ZDataTableSvc");
                assert_eq!(method, "GetRows");
                request.state = state;
                request.response = Some(json!({"returnObj":{"ZDataTable":[]},"parameters":{"morePages":false}}));
            }
            GuestReply::Action { action: HostAction::SharedQuery { query }, state } => {
                reads += 1;
                request.state = state;
                request.response = Some(if query.parameters.contains_key("after") {
                    json!({"rows":[row("UD01","c")]})
                } else { json!({"rows":[row("UD01","a"),row("UD02","b")]}) });
            }
            GuestReply::Definition { definition, completeness, diagnostics, .. } => {
                assert_eq!(completeness, Completeness::Collected, "{diagnostics:?}");
                assert!(diagnostics.is_empty());
                assert_eq!(reads, 12);
                assert_eq!(native_reads, 1);
                for table in ["ZLookupLink","ZLinkColumn","ZLookupField","ZDataField","ZKey","ZKeyField"] {
                    let values = definition.pointer(&format!("/definition/{table}")).unwrap().as_array().unwrap();
                    assert_eq!(values.len(), 2);
                    assert!(values.iter().all(|r| r["DataTableID"] == "UD01" && r["Value"] == "Unchanged Ω"));
                }
                println!("Packaged C02 {}: six tables, twelve paged reads, exact owner rows retained", plugin.hash);
                break;
            }
            other => panic!("Unexpected compiled C02 reply: {other:?}"),
        }
    }
}
