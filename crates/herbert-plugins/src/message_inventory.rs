//! MessageTemplate.GetList omits Company. Query only identity fields instead.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub(super) fn discover(
    r: GuestRequest,
    spec: &super::Family,
    family: usize,
    next: Option<String>,
) -> GuestReply {
    let cursor = r
        .cursor
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(json!({}));
    let global = cursor["messageGlobal"] == true;
    let next_branch = if global {
        next
    } else {
        Some(json!({"family":family,"messageGlobal":true}).to_string())
    };
    let failed = |diagnostic| GuestReply::Inventory {
        identities: vec![],
        next_cursor: next_branch.clone(),
        authoritative: false,
        diagnostics: vec![diagnostic],
    };
    let Some(size) = r.page_size.filter(|s| *s > 0) else {
        return failed(Diagnostic::new(
            "PAGE_SIZE_REQUIRED",
            "Supply a positive inventory page size.",
        ));
    };
    if let Some(error) = r.error {
        return failed(error);
    }
    let Some(response) = r.response else {
        let mut parameters = BTreeMap::from([
            (
                "company".into(),
                json!(if global { "" } else { &r.company }),
            ),
            ("custom".into(), json!(false)),
        ]);
        let mut criteria = vec![
            QueryCriterion {
                table: "Template".into(),
                field: "Company".into(),
                operator: "=".into(),
                parameter: "company".into(),
            },
            QueryCriterion {
                table: "Template".into(),
                field: "SystemFlag".into(),
                operator: "=".into(),
                parameter: "custom".into(),
            },
        ];
        if let Some(after) = cursor["after"].as_str() {
            parameters.insert("after".into(), json!(after));
            criteria.push(QueryCriterion {
                table: "Template".into(),
                field: "SysRowID".into(),
                operator: ">".into(),
                parameter: "after".into(),
            });
        }
        return GuestReply::Action {
            action: HostAction::Query {
                query: QuerySpec {
                    tables: vec![QueryTable {
                        schema: "Ice".into(),
                        table: "MessageTemplate".into(),
                        alias: "Template".into(),
                        company_field: None,
                        metadata: vec![],
                    }],
                    fields: [
                        "Company",
                        "MessageTemplateID",
                        "LangNameID",
                        "SystemFlag",
                        "SysRowID",
                    ]
                    .into_iter()
                    .map(|field| QueryField {
                        table: "Template".into(),
                        field: field.into(),
                        alias: field.into(),
                        expression: None,
                    })
                    .collect(),
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
            state: json!({"phase":"messageInventory"}),
        };
    };
    let Some(rows) = response["rows"].as_array() else {
        return failed(Diagnostic::new(
            "MESSAGE_INVENTORY_INVALID",
            "The message inventory omitted its rows.",
        ));
    };
    let mut identities = vec![];
    let mut seen = BTreeSet::new();
    for row in rows {
        if row["SystemFlag"] != false
            || row["Company"].as_str() != Some(if global { "" } else { &r.company })
        {
            return failed(Diagnostic::new(
                "MESSAGE_INVENTORY_INVALID",
                "The message inventory returned a System-marked or wrong-company row.",
            ));
        }
        match super::make_identity(spec, row, &r.company) {
            Ok(id) => {
                if !seen.insert(serde_json::to_string(&id).unwrap()) {
                    return failed(Diagnostic::new(
                        "MESSAGE_INVENTORY_INVALID",
                        "The message inventory repeats an identity.",
                    ));
                }
                identities.push(id);
            }
            Err(GuestReply::Failure { diagnostic }) => return failed(diagnostic),
            _ => unreachable!(),
        }
    }
    let next_cursor = if rows.len() >= size as usize {
        let Some(after) = rows
            .last()
            .and_then(|r| r["SysRowID"].as_str())
            .filter(|s| !s.is_empty() && cursor["after"].as_str() != Some(s))
        else {
            return failed(Diagnostic::new(
                "MESSAGE_INVENTORY_INVALID",
                "The message inventory page did not advance.",
            ));
        };
        Some(json!({"family":family,"messageGlobal":global,"after":after}).to_string())
    } else {
        next_branch
    };
    GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: true,
        diagnostics: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_projects_only_identity_pages_and_preserves_company() {
        let spec = super::super::specs()
            .into_iter()
            .find(|s| s.kind == "MessageTemplate")
            .unwrap();
        let mut r = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C25".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: Value::Null,
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = discover(r.clone(), &spec, 2, None)
        else {
            panic!()
        };
        assert_eq!(query.fields.len(), 5);
        assert!(
            query
                .fields
                .iter()
                .all(|f| !matches!(f.field.as_str(), "Body" | "Subject" | "*"))
        );
        r.response = Some(
            json!({"rows":[{"Company":"C","MessageTemplateID":"One","LangNameID":"enu","SystemFlag":false,"SysRowID":"1"},{"Company":"C","MessageTemplateID":"Two","LangNameID":"enu","SystemFlag":false,"SysRowID":"2"}]}),
        );
        let GuestReply::Inventory {
            identities,
            next_cursor,
            ..
        } = discover(r.clone(), &spec, 2, None)
        else {
            panic!()
        };
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].keys["Company"], "C");
        r.cursor = next_cursor;
        r.response = None;
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = discover(r.clone(), &spec, 2, None)
        else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "2");
        for row in [
            json!({"Company":"C","MessageTemplateID":"Stock","LangNameID":"enu","SystemFlag":true,"SysRowID":"3"}),
            json!({"Company":"Elsewhere","MessageTemplateID":"Foreign","LangNameID":"enu","SystemFlag":false,"SysRowID":"3"}),
        ] {
            r.response = Some(json!({"rows":[row]}));
            let GuestReply::Inventory {
                authoritative,
                identities,
                ..
            } = discover(r.clone(), &spec, 2, None)
            else {
                panic!()
            };
            assert!(!authoritative);
            assert!(identities.is_empty());
        }
    }
}
