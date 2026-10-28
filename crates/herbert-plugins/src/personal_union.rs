//! Combine personalization roots in one unsaved native BAQ. Keep the existing
//! identity, child-table, image and export readers after the combined page.
use super::*;
use base64::Engine;

const TABLES: &[&str] = &[
    "XXXDef",
    "ShellLayout",
    "ShellLayoutPersonal",
    "HomeUserOption",
    "FavFolder",
    "FavItems",
];

fn invalid(message: &str) -> GuestReply {
    fail("PERSONAL_UNION_INVALID", message)
}

pub(super) fn handle(mut request: GuestRequest) -> GuestReply {
    let selected: Vec<_> = specs()
        .into_iter()
        .filter(|s| s.category == "C13")
        .collect();
    if request.state["phase"] == "children" {
        return children(request, &selected);
    }
    if let Some(error) = request.error.take() {
        return GuestReply::Failure { diagnostic: error };
    }
    if request.state["phase"] == "metadata" {
        let Some(fields) = request
            .response
            .as_ref()
            .and_then(|v| v["returnObj"]["TableFieldList"].as_array())
        else {
            return invalid("The native field dictionary omitted its fields.");
        };
        let table = request.state["table"].as_str().unwrap_or("").to_owned();
        if fields.is_empty() || !fields.iter().any(|f| f["FieldName"] == "SysRowID") {
            return invalid("The native field dictionary has no row identity.");
        }
        if !request.state["metadata"].is_object() {
            request.state["metadata"] = json!({});
        }
        request.state["metadata"][table] = json!(fields);
        request.state["phase"] = Value::Null;
        request.response = None;
    }
    if request.state["phase"] == "union" {
        let Some(result) = request.response.as_ref().map(|v| &v["returnObj"]) else {
            return invalid("The combined query returned no result.");
        };
        if result["Errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty())
        {
            return invalid(&format!(
                "The combined query reported: {}",
                result["Errors"]
            ));
        }
        let Some(rows) = result["Results"].as_array() else {
            return invalid("The combined query omitted its result rows.");
        };
        let Some(size) = request.page_size else {
            return invalid("Supply a page size.");
        };
        if rows.len() > size as usize {
            return invalid("The combined query exceeded its requested page size.");
        }
        let mut groups: BTreeMap<(usize, String), Vec<Value>> = BTreeMap::new();
        let mut ids = Vec::new();
        let mut seen = BTreeSet::new();
        for source in rows {
            let Some(index) = source["RecordKind"].as_u64().map(|n| n as usize) else {
                return invalid("A combined row has no source kind.");
            };
            let Some(spec) = selected.get(index) else {
                return invalid("A combined row has an unknown source kind.");
            };
            let prefix = format!("{}_", spec.table);
            let mut row: Value = Value::Object(
                source
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter_map(|(key, value)| {
                        key.strip_prefix(&prefix)
                            .map(|key| (key.to_owned(), value.clone()))
                    })
                    .collect(),
            );
            // Native query reads expose SQL rowversion as UInt64. The union
            // projects its original eight bytes; restore that same wire value.
            for field in request.state["metadata"][&spec.table].as_array().unwrap() {
                if field["DataType"] != "timestamp" {
                    continue;
                }
                let name = field["FieldName"].as_str().unwrap();
                let Some(encoded) = row[name].as_str() else {
                    return invalid("A rowversion is not an encoded eight-byte value.");
                };
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
                    return invalid("A rowversion has invalid encoding.");
                };
                let Ok(bytes) = <[u8; 8]>::try_from(bytes.as_slice()) else {
                    return invalid("A rowversion has the wrong byte length.");
                };
                row[name] = json!(u64::from_be_bytes(bytes));
            }
            if crate::system_artifacts::marked(&row) {
                continue;
            }
            let id = match identity(spec, &row, &request.company) {
                Ok(id) => id,
                Err(error) => return error,
            };
            if !seen.insert(serde_json::to_string(&id).unwrap()) {
                return invalid("The combined page repeats an identity.");
            }
            ids.push(id);
            groups
                .entry((index, row["Company"].as_str().unwrap_or("").to_owned()))
                .or_default()
                .push(row);
        }
        let next = if rows.len() == size as usize {
            let last = rows.last().unwrap();
            let Some(id) = last["RecordID"].as_str().filter(|s| valid_guid(s)) else {
                return invalid("The combined page has no valid final row ID.");
            };
            let next = json!({"union":1,"family":last["RecordKind"],"after":id}).to_string();
            if request.cursor.as_ref() == Some(&next) {
                return invalid("The combined page did not advance.");
            }
            Some(next)
        } else {
            None
        };
        request.state["inventory"] = json!(GuestReply::Inventory {
            identities: ids,
            next_cursor: next,
            authoritative: true,
            diagnostics: vec![]
        });
        request.state["groups"] = json!(groups.into_iter().map(|((family,company),rows)| json!({"family":family,"company":company,"rows":rows})).collect::<Vec<_>>());
        request.state["group"] = json!(0);
        request.state["records"] = json!([]);
        request.state["phase"] = json!("children");
        request.response = None;
        return children(request, &selected);
    }
    for table in TABLES {
        if !request.state["metadata"][*table].is_array() {
            request.state["phase"] = json!("metadata");
            request.state["table"] = json!(table);
            return crate::native(
                "Ice.BO.BAQDesignerSvc",
                "GetFieldList",
                json!({"dataTableID":table,"schema":"Ice"}),
                false,
                request.state,
            );
        }
    }
    let arguments = match build(&request, &selected) {
        Ok(value) => value,
        Err(message) => return invalid(&message),
    };
    request.state["phase"] = json!("union");
    crate::native(
        "Ice.BO.DynamicQuerySvc",
        "Execute",
        arguments,
        false,
        request.state,
    )
}

/// Retain bulk child reads. Within a source family the union sorts native GUIDs,
/// so each company group has the same native interval used by table_batches.
fn children(mut request: GuestRequest, selected: &[TableFamily]) -> GuestReply {
    loop {
        let index = request.state["group"].as_u64().unwrap_or(0) as usize;
        let Some(group) = request.state["groups"].get(index).cloned() else {
            return GuestReply::InventoryData {
                records: serde_json::from_value(request.state["records"].clone()).unwrap(),
                inventory: Box::new(
                    serde_json::from_value(request.state["inventory"].clone()).unwrap(),
                ),
            };
        };
        let spec = &selected[group["family"].as_u64().unwrap() as usize];
        let roots = group["rows"].as_array().unwrap();
        if spec.children.is_empty() {
            let records = roots
                .iter()
                .map(|row| json!([identity(spec,row,&request.company).unwrap(),{"row":row}]));
            request.state["records"]
                .as_array_mut()
                .unwrap()
                .extend(records);
        } else {
            let mut inner = request.clone();
            inner.company = group["company"].as_str().unwrap().to_owned();
            let result = if let Some(state) = request.state.get("childState") {
                inner.state = state.clone();
                table_batches::resume(spec, inner)
            } else {
                let inventory = GuestReply::Inventory {
                    identities: vec![],
                    next_cursor: None,
                    authoritative: true,
                    diagnostics: vec![],
                };
                table_batches::start(spec, inner, roots, inventory)
            };
            match result {
                GuestReply::Action { action, state } => {
                    request.state["childState"] = state;
                    return GuestReply::Action {
                        action,
                        state: request.state,
                    };
                }
                GuestReply::InventoryData { records, .. } => request.state["records"]
                    .as_array_mut()
                    .unwrap()
                    .extend(records.into_iter().map(|v| json!(v))),
                other => return other,
            }
        }
        request.state.as_object_mut().unwrap().remove("childState");
        request.state["group"] = json!(index + 1);
        request.response = None;
        request.error = None;
    }
}

fn valid_guid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn literal(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(format!("N'{}'", s.replace('\'', "''"))),
        Value::Bool(b) => Ok(if *b { "1" } else { "0" }.into()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err("A native criterion has an unsupported value.".into()),
    }
}
fn lengths(table: &str) -> &[(&str, &str)] {
    match table {
        "XXXDef" => &[("Content", "HerbertContentBytes")],
        "ShellLayout" => &[
            ("ShellHomePage", "HerbertHomeBytes"),
            ("ShellUserOptions", "HerbertOptionsBytes"),
            ("FavoriteItems", "HerbertFavoritesBytes"),
        ],
        "ShellLayoutPersonal" => &[("ShellHomePage", "HerbertHomeBytes")],
        "HomeUserOption" => &[("ShellUserOptions", "HerbertOptionsBytes")],
        _ => &[],
    }
}

fn build(request: &GuestRequest, selected: &[TableFamily]) -> Result<Value, String> {
    let cursor: Value = request
        .cursor
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "The combined cursor is invalid.")?
        .unwrap_or(Value::Null);
    if !cursor.is_null()
        && (cursor["union"] != 1
            || cursor["family"]
                .as_u64()
                .is_none_or(|n| n >= selected.len() as u64)
            || !cursor["after"].as_str().is_some_and(valid_guid))
    {
        return Err("Restart this download to use the combined query cursor.".into());
    }
    let size = request
        .page_size
        .filter(|n| *n > 0)
        .ok_or("Supply a positive page size.")?;
    // Query IDs are local to Execute. No saved query is created or replaced.
    let query_id = "HerbertPersonalizationRead";
    let mut ds = json!({"DynamicQuery":[{"Company":request.company,"QueryID":query_id,"Description":"Unsaved personalization collection","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":false,"RowMod":"A"}],
        "QuerySubQuery":[],"QueryTable":[],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (branch, spec) in selected.iter().enumerate() {
        let alias = format!("Source{branch}");
        let sub = format!("00000000-0000-0000-0000-{:012}", branch + 1);
        let row = |extra: Value| {
            let mut value =
                json!({"Company":request.company,"QueryID":query_id,"SubQueryID":sub,"RowMod":"A"});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            value
        };
        let mut subquery = row(
            json!({"Name":alias,"Type":if branch==0 {"TopLevel"} else {"UnionAll"},"Seq":branch+1}),
        );
        if branch == 0 {
            subquery["OrderByOffSet"] = json!("0");
            subquery["OrderByFetch"] = json!(size.to_string());
        }
        ds["QuerySubQuery"].as_array_mut().unwrap().push(subquery);
        ds["QueryTable"].as_array_mut().unwrap().extend([
            row(json!({"TableID":alias,"DBSchemaName":"Ice","DBTableName":spec.table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),
            row(json!({"TableID":"Calculated","DBSchemaName":"","DBTableName":"","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))]);
        let mut fields = vec![
            (
                "RecordKind".to_owned(),
                "int".to_owned(),
                branch.to_string(),
            ),
            (
                "RecordID".into(),
                "uniqueidentifier".into(),
                format!("{alias}.SysRowID"),
            ),
        ];
        for table in TABLES {
            for field in request.state["metadata"][*table]
                .as_array()
                .ok_or("Missing native metadata.")?
            {
                let name = field["FieldName"].as_str().ok_or("Missing field name.")?;
                let ty = field["DataType"].as_str().ok_or("Missing field type.")?;
                if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err("Invalid dictionary field name.".into());
                }
                let formula = if *table != spec.table {
                    "NULL".to_owned()
                } else if ty == "nvarchar" || ty == "xml" {
                    format!("CONVERT(nvarchar(max), {alias}.{name})")
                } else if ty == "timestamp" {
                    format!("CONVERT(varbinary(8), {alias}.{name})")
                } else {
                    format!("{alias}.{name}")
                };
                fields.push((
                    format!("{table}_{name}"),
                    match ty {
                        "timestamp" => "varbinary",
                        "xml" => "nvarchar",
                        _ => ty,
                    }
                    .into(),
                    formula,
                ));
            }
            for (field, name) in lengths(table) {
                fields.push((
                    format!("{table}_{name}"),
                    "nvarchar".into(),
                    if *table == spec.table {
                        format!("CONVERT(nvarchar(max), DATALENGTH({alias}.{field}))")
                    } else {
                        "NULL".into()
                    },
                ));
            }
        }
        for (index, (name, ty, formula)) in fields.into_iter().enumerate() {
            ds["QueryField"].as_array_mut().unwrap().push(row(json!({"TableID":"Calculated","FieldName":name,"Alias":name,"DBSchemaName":"","DBTableName":"","DBFieldName":"","DataType":ty,"FieldFormat":"","FieldLabel":name,"Seq":index+1,"IsCalculated":true,"Formula":formula,"Updatable":false})));
        }
        let mut criteria: Vec<(&str, &str, String, String, String, String)> = vec![];
        if spec.company {
            let mut companies = BTreeSet::from([String::new(), request.company.clone()]);
            if let Some(tenant) = request.state["_personalTenant"]
                .as_str()
                .filter(|s| !s.is_empty())
            {
                companies.insert(format!("T_{tenant}"));
            }
            let count = companies.len();
            for (index, company) in companies.into_iter().enumerate() {
                criteria.push((
                    "Company",
                    "=",
                    literal(&json!(company))?,
                    if index == 0 { "And" } else { "Or" }.into(),
                    if index == 0 { "(" } else { "" }.into(),
                    if index + 1 == count { ")" } else { "" }.into(),
                ));
            }
        }
        for (field, value) in spec.filters.iter().chain(spec.discovery_filters.iter()) {
            criteria.push((
                field,
                "=",
                literal(value)?,
                "And".into(),
                String::new(),
                String::new(),
            ));
        }
        if let Some(family) = cursor["family"].as_u64() {
            if (branch as u64) < family {
                criteria.push((
                    "SysRowID",
                    "=",
                    "'00000000-0000-0000-0000-000000000000'".into(),
                    "And".into(),
                    String::new(),
                    String::new(),
                ));
                criteria.push((
                    "SysRowID",
                    "<>",
                    "'00000000-0000-0000-0000-000000000000'".into(),
                    "And".into(),
                    String::new(),
                    String::new(),
                ));
            } else if branch as u64 == family {
                criteria.push((
                    "SysRowID",
                    ">",
                    literal(&cursor["after"])?,
                    "And".into(),
                    String::new(),
                    String::new(),
                ));
            }
        }
        for (index, (field, op, value, and_or, left, right)) in criteria.into_iter().enumerate() {
            ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0000-{:04}-{:012}",branch+1,index+1),"TableID":alias,"FieldName":field,"CompOp":op,"RValue":value,"IsConst":true,"CriteriaType":2,"Seq":index+1,"AndOr":if index==0 {""} else {&and_or},"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
        }
        if branch == 0 {
            for (index, field) in ["RecordKind", "RecordID"].iter().enumerate() {
                ds["QuerySortBy"].as_array_mut().unwrap().push(row(
                    json!({"TableID":"Calculated","FieldName":field,"Seq":index+1,"IsAsc":true}),
                ));
            }
        }
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn combined_page_preserves_chunk_rows_under_the_exact_parent() {
        let root = json!({"Company":"C","ProductID":"EP","TypeCode":"KineticGridLayout","Key1":"alice","Key2":"Form","Key3":"Grid","CGCCode":"","SysRowID":"11111111-1111-1111-1111-111111111111","SystemFlag":false});
        let spec = specs().into_iter().find(|s| s.category == "C13").unwrap();
        let id = identity(&spec, &root, "C").unwrap();
        let inventory = GuestReply::Inventory {
            identities: vec![id.clone()],
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        };
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C13".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({"phase":"children","group":0,"groups":[{"family":0,"company":"C","rows":[root]}],"records":[],"inventory":inventory}),
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.joins.len(), 7);
        assert_eq!(query.parameters["batchCompany"], "C");
        let mut child = root.clone();
        child["SysRowID"] = json!("22222222-2222-2222-2222-222222222222");
        child["Chunk"] = json!("<Fixture/>");
        request.state = state;
        request.response = Some(json!({"rows":[child]}));
        let GuestReply::InventoryData { records, .. } = handle(request) else {
            panic!()
        };
        assert_eq!(records[0].0, id);
        assert_eq!(
            records[0].1["children"]["XXXChunk"][0]["Chunk"],
            "<Fixture/>"
        );
        assert_eq!(records[0].1["errors"], json!([]));
    }
    #[test]
    fn combined_query_preserves_scope_types_lengths_and_keyset() {
        let mut metadata = json!({});
        for table in TABLES {
            metadata[*table] = json!([
                {"FieldName":"SysRowID","DataType":"uniqueidentifier"},
                {"FieldName":"Content","DataType":"nvarchar"},
                {"FieldName":"SysRevID","DataType":"timestamp"}
            ]);
        }
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C13".into(),
            company: "C'1".into(),
            identity: None,
            cursor: Some(
                json!({"union":1,"family":1,"after":"11111111-1111-1111-1111-111111111111"})
                    .to_string(),
            ),
            page_size: Some(1000),
            state: json!({"metadata":metadata,"_personalTenant":"123"}),
            response: None,
            error: None,
        };
        let selected: Vec<_> = specs()
            .into_iter()
            .filter(|s| s.category == "C13")
            .collect();
        let args = build(&request, &selected).unwrap();
        let ds = &args["queryDS"];
        assert_eq!(ds["QuerySubQuery"][0]["OrderByFetch"], "1000");
        assert_eq!(ds["QuerySubQuery"][0]["OrderByOffSet"], "0");
        assert_eq!(ds["QuerySubQuery"].as_array().unwrap().len(), 7);
        assert!(
            ds["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["RValue"] == "N'C''1'")
        );
        assert!(
            ds["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["RValue"] == "N'T_123'")
        );
        assert!(
            ds["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["CompOp"] == ">" && r["FieldName"] == "SysRowID")
        );
        assert!(
            ds["QueryField"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["Formula"] == "CONVERT(nvarchar(max), DATALENGTH(Source0.Content))")
        );
        assert!(
            ds["QueryField"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["Alias"] == "RecordID" && r["DataType"] == "uniqueidentifier")
        );
        assert_eq!(ds["DynamicQuery"][0]["Updatable"], false);
        let mut old = request.clone();
        old.cursor = Some(json!({"family":0,"after":null}).to_string());
        assert!(build(&old, &selected).is_err());
    }
    #[test]
    fn native_query_errors_do_not_report_an_empty_success() {
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C13".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(1000),
            state: json!({"phase":"union"}),
            response: Some(
                json!({"returnObj":{"Results":[],"Errors":[{"ErrorText":"native failure"}]}}),
            ),
            error: None,
        };
        assert!(matches!(handle(request), GuestReply::Failure { .. }));
    }
}
