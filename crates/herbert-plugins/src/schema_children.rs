//! Custom schema additions can belong to a stock table. Retain its keys as
//! context and read only custom children. Never fetch its system definition.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
const TABLES: &[&str] = &[
    "ZLookupLink",
    "ZLinkColumn",
    "ZLookupField",
    "ZDataField",
    "ZKey",
    "ZKeyField",
];
fn failure(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
pub fn start(mut request: GuestRequest) -> GuestReply {
    let id = request.identity.as_ref().unwrap();
    request.state = json!({"phase":"schemaCustomChildren","index":0,"definition":{"ZDataTable":[{"SystemCode":id.keys["SystemCode"],"DataTableID":id.keys["DataTableID"]}],"herbertSchemaContext":"Parent keys identify the owner of these custom additions; the system table definition is not included."}});
    request.response = None;
    request.error = None;
    complete(request)
}
pub fn complete(mut request: GuestRequest) -> GuestReply {
    if let Some(diagnostic) = request.error.take() {
        return GuestReply::Failure { diagnostic };
    }
    let Some(mut index) = request.state["index"]
        .as_u64()
        .map(|v| v as usize)
        .filter(|v| *v < TABLES.len())
    else {
        return failure(
            "INVALID_PLUGIN_STATE",
            "The custom schema child saved progress is invalid.",
        );
    };
    let id = request.identity.as_ref().unwrap();
    let Some(size) = request.page_size.filter(|n| *n > 0) else {
        return failure("PAGE_SIZE_REQUIRED", "Supply a positive schema batch size.");
    };
    if let Some(response) = request.response.take() {
        let Some(rows) = response["rows"].as_array() else {
            return failure(
                "SCHEMA_CHILDREN_UNVERIFIED",
                "The custom schema query omitted its rows.",
            );
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut selected = Vec::new();
        for row in rows {
            if row["SystemFlag"].as_bool() != Some(false) {
                return failure(
                    "SCHEMA_SYSTEM_FILTER_FAILED",
                    "The schema query returned a system row or omitted its ownership flag.",
                );
            }
            if ["SystemCode", "DataTableID"]
                .iter()
                .any(|key| row[*key].as_str().is_none_or(str::is_empty))
            {
                return failure(
                    "SCHEMA_CHILD_IDENTITY_MISSING",
                    "A custom schema row omitted its table identity.",
                );
            }
            let Some(row_id) = row["SysRowID"].as_str().filter(|v| !v.is_empty()) else {
                return failure(
                    "SCHEMA_CHILD_IDENTITY_MISSING",
                    "A custom schema row omitted its Epicor row identity.",
                );
            };
            if !seen.insert(row_id) {
                return failure(
                    "SCHEMA_CHILD_DUPLICATE",
                    "The custom schema query repeated an Epicor row.",
                );
            }
            if ["SystemCode", "DataTableID"].iter().all(|key| {
                row[*key]
                    .as_str()
                    .unwrap()
                    .eq_ignore_ascii_case(&id.keys[*key])
            }) {
                selected.push(row.clone());
            }
        }
        if request.state["definition"][TABLES[index]].is_null() {
            request.state["definition"][TABLES[index]] = json!([]);
        }
        request.state["definition"][TABLES[index]]
            .as_array_mut()
            .unwrap()
            .extend(selected);
        if rows.len() == size as usize {
            let after = &rows.last().unwrap()["SysRowID"];
            if request.state["after"] == *after {
                return failure("SCHEMA_PAGING_STALLED", "The schema batch did not advance.");
            }
            request.state["after"] = after.clone();
            return complete(request);
        }
        request.state["after"] = Value::Null;
        index += 1;
        if index == TABLES.len() {
            let definition = request.state["definition"].clone();
            if TABLES
                .iter()
                .all(|table| definition[*table].as_array().unwrap().is_empty())
            {
                return failure(
                    "CUSTOM_SCHEMA_NOT_FOUND",
                    "No custom table definition or custom schema additions were found for this identity.",
                );
            }
            request.state = json!({"phase":"root"});
            // Every custom-child query has reached its final page. Preserve that
            // completion evidence when handing the assembled definition to the
            // same validator used by native exact-schema responses.
            request.response = Some(json!({"returnObj":definition,"parameters":{"morePages":false}}));
            return crate::families::handle(request);
        }
        request.state["index"] = json!(index);
    }
    let mut parameters = BTreeMap::from([("custom".into(), json!(false))]);
    let mut criteria = vec![QueryCriterion {
        table: "SchemaSource".into(),
        field: "SystemFlag".into(),
        operator: "=".into(),
        parameter: "custom".into(),
    }];
    if let Some(after) = request.state["after"].as_str() {
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: "SchemaSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    GuestReply::Action {
        action: HostAction::SharedQuery {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Ice".into(),
                    table: TABLES[index].into(),
                    alias: "SchemaSource".into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields: vec![QueryField {
                    table: "SchemaSource".into(),
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
                page_size: Some(size),
                page: 1,
            },
        },
        state: request.state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(table: &str) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "ud-schema".into(),
            company: "Example".into(),
            identity: Some(Identity {
                category: "ud-schema".into(),
                kind: "UDTableSchema".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([
                    ("SystemCode".into(), "ICE".into()),
                    ("DataTableID".into(), table.into()),
                ]),
            }),
            cursor: None,
            page_size: Some(2),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    fn row(table: &str, id: &str) -> Value {
        json!({"SystemCode":"Ice","DataTableID":table,"FieldName":format!("Custom_{id}_c"),"LookupID":id,"LinkID":id,"KeyID":id,"Seq":1,"SystemFlag":false,"SysRowID":format!("00000000-0000-0000-0000-00000000000{id}"),"Value":"Unchanged Ω"})
    }
    #[test]
    fn different_tables_share_one_batch_and_keep_only_their_own_rows() {
        let GuestReply::Action { action: first, .. } = start(request("UD01")) else {
            panic!()
        };
        let GuestReply::Action { action: second, .. } = start(request("UD02")) else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
        let mut r = request("UD01");
        let mut reply = start(r.clone());
        for table in TABLES {
            let GuestReply::Action {
                action: HostAction::SharedQuery { query },
                state,
            } = reply
            else {
                panic!()
            };
            assert_eq!(query.tables[0].table, *table);
            assert_eq!(query.page_size, Some(2));
            assert!(!query.parameters.contains_key("table"));
            r.state = state;
            r.response = Some(json!({"rows":[row("UD01","a"),row("UD02","b")]}));
            let GuestReply::Action {
                action: HostAction::SharedQuery { query },
                state,
            } = complete(r.clone())
            else {
                panic!()
            };
            assert_eq!(
                query.parameters["after"],
                "00000000-0000-0000-0000-00000000000b"
            );
            r.state = state;
            r.response = Some(json!({"rows":[row("UD01","c")]}));
            reply = complete(r.clone());
        }
        let GuestReply::Definition {
            definition,
            completeness,
            diagnostics,
            ..
        } = reply
        else {
            panic!("No companion download is needed: {reply:?}")
        };
        assert_eq!(completeness, Completeness::Collected, "{diagnostics:?}");
        for table in TABLES {
            // Family output wraps the native definition for local extraction.
            let values = definition.pointer(&format!("/definition/{table}")).unwrap();
            assert_eq!(values.as_array().unwrap().len(), 2);
            assert_eq!(values[0]["Value"], "Unchanged Ω");
            assert_eq!(
                values[1]["SysRowID"],
                "00000000-0000-0000-0000-00000000000c"
            );
        }
    }
    #[test]
    fn invalid_or_refused_batches_do_not_become_successful_empty_results() {
        for rows in [
            json!([row("UD01", "a"), row("UD01", "a")]),
            json!([{"SystemFlag":true}]),
            json!([{"SystemFlag":false}]),
        ] {
            let mut r = request("UD01");
            let GuestReply::Action { state, .. } = start(r.clone()) else {
                panic!()
            };
            r.state = state;
            r.response = Some(json!({"rows":rows}));
            assert!(matches!(complete(r), GuestReply::Failure { .. }));
        }
        let mut r = request("UD01");
        let GuestReply::Action { state, .. } = start(r.clone()) else {
            panic!()
        };
        r.state = state;
        r.error = Some(Diagnostic::new("ACCESS_DENIED", "Refused."));
        assert!(
            matches!(complete(r),GuestReply::Failure{diagnostic}if diagnostic.code=="ACCESS_DENIED")
        );
    }
}
