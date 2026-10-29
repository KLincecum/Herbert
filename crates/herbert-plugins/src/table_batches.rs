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
        let spec = specs().into_iter().find(|s| s.category == "classic-customizations").unwrap();
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
            category: "classic-customizations".into(),
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
    fn legacy_discovery_fixture(mode: &str, family: &str) -> (GuestReply, BTreeMap<String, usize>) {
        let root = |like_id: &str, id: &str| json!({"LikeID":like_id,"ContextTypeCode":"OwnedType","Description":"DO NOT EMIT STOCK BODY","SystemFlag":true,"SysRowID":id});
        let roots = vec![root("Owned.Stock","11111111-1111-1111-1111-111111111111"),root("Owned.PureStock","22222222-2222-2222-2222-222222222222")];
        let mut request = GuestRequest { abi: PLUGIN_ABI, operation:"discover".into(), category:"searches-and-navigation".into(), company:"C".into(), identity:None,
            cursor:Some(json!({"searchVersion":1,"stream":3,"cursor":null}).to_string()),page_size:Some(2),
            state:json!({"_hostPreparePage":true,"_pluginSettings":{"definitionFamily":family,"searchIdPrefix":"Missing"}}),
            response:Some(json!({"rows":roots})),error:None };
        let mut reply=crate::search_pages::handle(request.clone());let mut calls=BTreeMap::new();
        while let GuestReply::Action {action,state}=reply {
            let query=match action {HostAction::Query {query}|HostAction::SharedQuery {query}=>query,_=>panic!("Expected a native page-bound child query")};
            let table=query.tables.iter().find(|table|table.alias=="DefinitionSource").unwrap().table.clone();
            assert_eq!(query.page_size,Some(2));assert_eq!(query.joins.len(),2);
            assert!(query.joins.iter().all(|join|join.left_field==join.right_field&&["LikeID","ContextTypeCode"].contains(&join.left_field.as_str())));
            assert!(!query.criteria.iter().any(|criterion|criterion.table=="BatchParent"&&criterion.field=="SystemFlag"));
            let count=calls.entry(table.clone()).or_insert(0);*count+=1;
            request.state=state;request.state["_hostPreparePage"]=json!(true);request.error=None;
            let rows=if table=="ContextMenuItem"&&*count==1&&mode=="custom" {
                vec![json!({"LikeID":"Owned.Stock","ContextTypeCode":"OwnedType","ContextMenuID":"OwnedChild","ContextMenuText":"Ω authored & child","ProcessType":"BaseForm","SystemFlag":false,"SysRowID":"44444444-4444-4444-4444-444444444444"}),
                     json!({"LikeID":"Owned.Stock","ContextTypeCode":"OwnedType","ContextMenuID":"StockChild","SystemFlag":true,"SysRowID":"55555555-5555-5555-5555-555555555555"})]
            }else{vec![]};
            request.response=Some(json!({"rows":rows}));
            if table=="ContextMenuItem"&&*count==1&&mode=="failure" {request.response=None;request.error=Some(Diagnostic::new("OWNED_CHILD_READ_FAILED","Original child read failure"));}
            reply=crate::search_pages::handle(request.clone());
        }
        assert_eq!(calls.keys().cloned().collect::<BTreeSet<_>>(),crate::search_archive::CONTEXT_CHILDREN.iter().map(|table|table.to_string()).collect());
        let GuestReply::PreparedPage {state,next_cursor,..}=reply else{panic!("The full discovery and child batches must produce a prepared page")};
        assert!(next_cursor.is_some(),"Full raw root page continuation must survive selection");
        let mut materialize=request;materialize.operation="materialize".into();materialize.response=None;materialize.error=None;materialize.state=state;
        (crate::search_pages::handle(materialize),calls)
    }

    #[test]
    fn legacy_stock_parent_discovery_batches_finish_materialize_keep_only_custom_child_and_parent_keys() {
        let (reply,calls)=legacy_discovery_fixture("custom","legacyContextMenu");
        assert_eq!(calls["ContextMenuItem"],2,"A full child page must use its original continuation");
        let GuestReply::InventoryData {records,inventory}=reply else{panic!()};assert_eq!(records.len(),1);
        let (identity,definition)=&records[0];assert_eq!(identity.keys["LikeID"],"Owned.Stock");
        assert_eq!(definition["ContextMenu"][0],json!({"LikeID":"Owned.Stock","ContextTypeCode":"OwnedType","BitFlag":0,"RowMod":""}));
        assert_eq!(definition["ContextMenuItem"].as_array().unwrap().len(),1);assert_eq!(definition["ContextMenuItem"][0]["ContextMenuText"],"Ω authored & child");
        assert!(definition["herbertParentContext"].is_string());assert!(definition["herbertSources"]["files"][0]["text"].as_str().unwrap().contains("Ω authored &amp; child"));
        assert!(!serde_json::to_string(definition).unwrap().contains("DO NOT EMIT STOCK BODY"));assert!(!serde_json::to_string(definition).unwrap().contains("StockChild"));
        let GuestReply::Inventory {identities,authoritative,diagnostics,next_cursor}=*inventory else{panic!()};assert_eq!(identities.len(),1);assert!(authoritative&&diagnostics.is_empty()&&next_cursor.is_some());
    }

    #[test]
    fn legacy_pure_stock_graph_is_inspected_before_verified_empty_output() {
        let (reply,calls)=legacy_discovery_fixture("pure","all");assert_eq!(calls.len(),crate::search_archive::CONTEXT_CHILDREN.len());
        let GuestReply::InventoryData {records,inventory}=reply else{panic!()};assert!(records.is_empty());
        let GuestReply::Inventory {identities,authoritative,diagnostics,next_cursor}=*inventory else{panic!()};assert!(identities.is_empty()&&authoritative&&diagnostics.is_empty()&&next_cursor.is_some());
    }

    #[test]
    fn legacy_stock_parent_child_failure_is_identity_bound_when_selected_and_silent_when_not() {
        // Selected: the child failure stays identity-bound and coverage incomplete.
        let (reply,_)=legacy_discovery_fixture("failure","legacyContextMenu");
        let GuestReply::InventoryData {inventory,..}=reply else{panic!()};
        let GuestReply::Inventory {authoritative,diagnostics,next_cursor,..}=*inventory else{panic!()};assert!(!authoritative&&next_cursor.is_some());
        assert_eq!(diagnostics.len(),2);assert!(diagnostics.iter().all(|error|error.code=="OWNED_CHILD_READ_FAILED"&&error.identity.as_ref().is_some_and(|identity|identity.kind=="PersistedContextMenu")));
        // Unselected family: The prefix rule keeps excluded rows silent.
        let (reply,_)=legacy_discovery_fixture("failure","quickSearch");
        let GuestReply::InventoryData {records,inventory}=reply else{panic!()};assert!(records.is_empty());
        let GuestReply::Inventory {identities,authoritative,diagnostics,next_cursor}=*inventory else{panic!()};
        assert!(identities.is_empty()&&authoritative&&diagnostics.is_empty()&&next_cursor.is_some(),"{diagnostics:?}");
    }

    #[test]
    fn legacy_root_exception_requires_exact_searches_and_navigation_native_full_graph_and_preserves_other_stock_exclusions() {
        let stock=json!({"SystemFlag":true});let mut spec=specs().into_iter().find(|spec|spec.category=="searches-and-navigation"&&spec.table=="ContextMenu").unwrap();
        assert!(!inventory_root_allowed(&spec,&stock),"A plain ContextMenu family without its child graph stays excluded");
        spec.filters.clear();spec.children=crate::search_archive::CONTEXT_CHILDREN.iter().map(|table|Child {table:(*table).into(),links:["LikeID","ContextTypeCode"].into_iter().map(|key|(key.into(),key.into())).collect(),filters:BTreeMap::new()}).collect();
        assert!(inventory_root_allowed(&spec,&stock));let complete=spec.clone();
        spec.children.pop();assert!(!inventory_root_allowed(&spec,&stock));spec=complete.clone();spec.children[0].links.insert("LikeID".into(),"OtherKey".into());assert!(!inventory_root_allowed(&spec,&stock));
        spec=complete.clone();spec.category="classic-customizations".into();assert!(!inventory_root_allowed(&spec,&stock));spec=complete.clone();spec.table="QuickSearch".into();assert!(!inventory_root_allowed(&spec,&stock));
        spec=complete;spec.schema="Other".into();assert!(!inventory_root_allowed(&spec,&stock));assert!(inventory_root_allowed(&spec,&json!({"SystemFlag":false})),"Existing custom-root behavior stays unchanged");
    }

}

pub(super) fn resume(spec: &TableFamily, mut request: GuestRequest) -> GuestReply {
    let index = request.state["child"].as_u64().unwrap_or(0) as usize;
    let Some(child) = spec.children.get(index) else {
        return fail(
            "INVALID_PLUGIN_STATE",
            "The child batch saved progress is invalid.",
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
        .filter(|row| inventory_root_allowed(spec, row))
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
