//! Read child tables for a page of parents, instead of once per parent.
use super::*;

pub(super) fn start(
    spec: &TableFamily,
    mut request: GuestRequest,
    roots: &[Value],
    inventory: GuestReply,
) -> GuestReply {
    request.state = json!({"phase":"inventoryChildren","roots":roots,"inventory":inventory,"child":0,"after":null,"tables":{},"errors":[]});
    next(spec, request)
}

fn next(spec: &TableFamily, request: GuestRequest) -> GuestReply {
    let index = request.state["child"].as_u64().unwrap_or(0) as usize;
    let Some(child) = spec.children.get(index) else {
        return finish(spec, request);
    };
    let roots = request.state["roots"].as_array().unwrap();
    let Some(first) = roots.first().and_then(|r| r[&spec.order].as_str()) else {
        return fail(
            "PAGING_UNVERIFIED",
            "The parent batch has no starting row ID.",
        );
    };
    let Some(last) = roots.last().and_then(|r| r[&spec.order].as_str()) else {
        return fail(
            "PAGING_UNVERIFIED",
            "The parent batch has no ending row ID.",
        );
    };
    let (mut query, shared) = match query(
        spec,
        &request,
        &child.table,
        BTreeMap::new(),
        false,
        Value::Null,
    ) {
        GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } => (query, false),
        GuestReply::Action {
            action: HostAction::SharedQuery { query },
            ..
        } => (query, true),
        other => return other,
    };
    query.tables.push(QueryTable {
        schema: spec.schema.clone(),
        table: spec.table.clone(),
        alias: "BatchParent".into(),
        company_field: None,
        metadata: vec![],
    });
    for (field, parent) in &child.links {
        query.joins.push(QueryJoin {
            left_table: "DefinitionSource".into(),
            left_field: field.clone(),
            right_table: "BatchParent".into(),
            right_field: parent.clone(),
            kind: "Inner".into(),
        });
    }
    // Bind the same parent interval and ownership filters as the inventory page.
    // New child pages keep these bounds, so they cannot collect unrelated parents.
    for (field, operator, value) in [
        (spec.order.as_str(), ">=", json!(first)),
        (spec.order.as_str(), "<=", json!(last)),
    ]
    .into_iter()
    .chain(
        spec.filters
            .iter()
            .chain(spec.discovery_filters.iter())
            .map(|(field, value)| (field.as_str(), "=", value.clone())),
    )
    .chain(spec.excluded_filters.iter().flat_map(|(field, values)| {
        values
            .iter()
            .map(move |value| (field.as_str(), "<>", value.clone()))
    })) {
        let parameter = format!("batchParent{}", query.parameters.len());
        query.criteria.push(QueryCriterion {
            table: "BatchParent".into(),
            field: field.into(),
            operator: operator.into(),
            parameter: parameter.clone(),
        });
        query.parameters.insert(parameter, value);
    }
    if spec.company {
        let parameter = "batchCompany".to_string();
        query.criteria.push(QueryCriterion {
            table: "BatchParent".into(),
            field: "Company".into(),
            operator: "=".into(),
            parameter: parameter.clone(),
        });
        query
            .parameters
            .insert(parameter, roots[0]["Company"].clone());
    }
    if let Some(after) = request.state["after"].as_str() {
        query.criteria.push(QueryCriterion {
            table: "DefinitionSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "batchAfter".into(),
        });
        query.parameters.insert("batchAfter".into(), json!(after));
    }
    query.order = vec![QueryOrder {
        field: "SysRowID".into(),
        descending: false,
    }];
    query.page_size = request.page_size;
    GuestReply::Action {
        action: if shared {
            HostAction::SharedQuery { query }
        } else {
            HostAction::Query { query }
        },
        state: request.state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kinetic_child_batch_accepts_and_keeps_shared_queries() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "PersistedKineticApplicationFile")
            .unwrap();
        let roots = vec![
            json!({"Company":"C","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"App","Key2":"App/events.jsonc","Key3":"","CGCCode":"","SysRowID":"root1","SystemFlag":false}),
        ];
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: spec.category.clone(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: Value::Null,
            response: None,
            error: None,
        };
        let inventory = GuestReply::Inventory {
            identities: vec![],
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        };
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            ..
        } = start(&spec, request, &roots, inventory)
        else {
            panic!("A Kinetic file page must produce a shared child batch without trapping.");
        };
        assert_eq!(query.page_size, Some(2));
        assert_eq!(query.joins.len(), spec.children[0].links.len());
    }

    #[test]
    fn child_requests_scale_with_pages_not_parent_count() {
        let spec = specs().into_iter().find(|s| s.category == "C09").unwrap();
        let root = |key: &str, id: &str| json!({"Company":"C","ProductID":"EP","TypeCode":"Customization","Key1":key,"Key2":"Form","Key3":"","CGCCode":"","SysRowID":id,"SystemFlag":false});
        let roots = vec![root("A", "root1"), root("B", "root2")];
        let inventory = GuestReply::Inventory {
            identities: roots
                .iter()
                .map(|row| identity(&spec, row, "C").unwrap())
                .collect(),
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        };
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C09".into(),
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
            state,
        } = start(&spec, request.clone(), &roots, inventory)
        else {
            panic!()
        };
        assert_eq!(query.page_size, Some(2));
        assert_eq!(query.joins.len(), spec.children[0].links.len());
        assert!(query.criteria.iter().any(|c| c.table == "BatchParent"
            && c.field == "SystemFlag"
            && query.parameters[&c.parameter] == false));
        let child = |key: &str, id: &str| {
            let mut row = root(key, id);
            row["Chunk"] = json!("preserve bytes");
            row
        };
        request.state = state;
        request.response = Some(json!({"rows":[child("A","child1"),child("B","child2")]}));
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = resume(&spec, request.clone())
        else {
            panic!()
        };
        assert_eq!(query.parameters["batchAfter"], "child2");
        request.state = state;
        request.response = Some(json!({"rows":[child("A","child3")]}));
        let GuestReply::InventoryData { records, .. } = resume(&spec, request) else {
            panic!("The last batch must finish without a per-parent read.")
        };
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].1["children"]["XXXChunk"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            records[1].1["children"]["XXXChunk"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            records[0].1["children"]["XXXChunk"][1]["Chunk"],
            "preserve bytes"
        );
    }
}

pub(super) fn resume(spec: &TableFamily, mut request: GuestRequest) -> GuestReply {
    let index = request.state["child"].as_u64().unwrap_or(0) as usize;
    let Some(child) = spec.children.get(index) else {
        return fail(
            "INVALID_PLUGIN_STATE",
            "The child batch checkpoint is invalid.",
        );
    };
    let mut more = false;
    if let Some(error) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(error));
    } else if let Some(rows) = request.response.as_ref().and_then(|r| r["rows"].as_array()) {
        if !request.state["tables"][&child.table].is_array() {
            request.state["tables"][&child.table] = json!([]);
        }
        request.state["tables"][&child.table]
            .as_array_mut()
            .unwrap()
            .extend(rows.iter().cloned());
        if request
            .page_size
            .is_some_and(|size| rows.len() >= size as usize)
        {
            if let Some(after) = rows
                .last()
                .and_then(|row| row["SysRowID"].as_str())
                .filter(|after| Some(*after) != request.state["after"].as_str())
            {
                request.state["after"] = json!(after);
                more = true;
            } else {
                request.state["errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(Diagnostic::new(
                        "PAGING_UNVERIFIED",
                        "The child batch did not provide an advancing row ID."
                    )));
            }
        }
    } else {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(Diagnostic::new(
                "CHILDREN_UNVERIFIED",
                "The child batch omitted its rows."
            )));
    }
    if !more {
        request.state["child"] = json!(index + 1);
        request.state["after"] = Value::Null;
    }
    request.response = None;
    next(spec, request)
}

fn finish(spec: &TableFamily, request: GuestRequest) -> GuestReply {
    let roots = request.state["roots"].as_array().unwrap();
    let mut records: Vec<_> = roots
        .iter()
        .filter(|row| !crate::system_artifacts::marked(row))
        .filter_map(|row| {
            identity(spec, row, &request.company).ok().map(|id| {
                (
                    id,
                    json!({"row":row,"children":{},"errors":request.state["errors"]}),
                )
            })
        })
        .collect();
    for child in &spec.children {
        // Index each table once. Do not scan all children again for every parent.
        let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for row in request.state["tables"][&child.table]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| !crate::system_artifacts::marked(row))
        {
            let key = serde_json::to_string(
                &child
                    .links
                    .keys()
                    .map(|field| &row[field])
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            groups.entry(key).or_default().push(row.clone());
        }
        for (_, data) in &mut records {
            let key = serde_json::to_string(
                &child
                    .links
                    .values()
                    .map(|field| &data["row"][field])
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            data["children"][&child.table] = json!(groups.remove(&key).unwrap_or_default());
        }
        if !groups.is_empty() {
            for (_, data) in &mut records {
                data["errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(Diagnostic::new(
                        "CHILD_IDENTITY_MISMATCH",
                        "A child batch returned rows outside its downloaded parent inventory."
                    )));
            }
        }
    }
    let inventory = serde_json::from_value(request.state["inventory"].clone()).unwrap();
    GuestReply::InventoryData {
        records,
        inventory: Box::new(inventory),
    }
}
