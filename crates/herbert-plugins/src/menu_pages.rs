//! Read persisted menus in pages. Keep stock state separate from custom exports.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn prefix(request: &GuestRequest) -> Result<String, Diagnostic> {
    match request.state.pointer("/_pluginSettings/menuIdPrefix") {
        None=>Ok(String::new()),Some(Value::String(value))=>Ok(value.clone()),
        Some(_)=>Err(Diagnostic::new("INVALID_SETTING","Menu ID starts with must be a string.")),
    }
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings)=from.get("_pluginSettings") {state["_pluginSettings"]=settings.clone();}state
}
fn row_key(row: &Value) -> (String,String) {
    (row["Company"].as_str().unwrap().into(),row["MenuID"].as_str().unwrap().into())
}
fn valid_revision(value: &Value) -> bool {
    value.as_u64().is_some() || value.as_i64().is_some() || value.as_str().is_some_and(|text|text.parse::<u64>().is_ok() || text.parse::<i64>().is_ok())
}
fn same_witness(row: &Value, witness: &Value) -> bool {
    row["SysRowID"]==witness["SysRowID"] && row["SysRevID"]==witness["SysRevID"]
        && row["ParentMenuID"]==witness["ParentMenuID"] && row["SystemFlag"]==witness["SystemFlag"] && row["CompanyVisibility"]==witness["CompanyVisibility"]
}
fn scoped_row(row: &Value, request: &GuestRequest) -> bool {
    let owner=row["Company"].as_str().unwrap_or("");
    owner==request.company || owner.is_empty() || row["CompanyVisibility"]==10 || row["CompanyVisibility"]==5 && request.state["companies"].as_array().is_some_and(|peers|peers.iter().any(|peer|peer==owner))
}
fn source_errors(row: &Value, company: &str) -> Vec<Diagnostic> {
    let mut errors=Vec::new();
    if !matches!(row["CompanyVisibility"].as_i64(),Some(0|5|10)) {
        errors.push(Diagnostic::new("MENU_VISIBILITY_UNKNOWN","The menu has an unknown company visibility. Its source is preserved; review its restore scope."));
    }
    let mut definition=json!({"Menu":[native_row(row)]});
    if let Err(error)=append(&mut definition,"validation.xml") {errors.push(error);}
    for error in &mut errors {error.identity=menu_identity(row,company).ok();}
    errors
}

pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.Lib.SessionModSvc", "GetCurrentValuesFromServer"),
        ("Ice.BO.BAQDesignerSvc", "GetFieldList"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
    ]
    .into_iter()
    .map(|(s, m)| (s.into(), m.into()))
    .collect()
}
pub fn response_policies(methods: &[(String, String)]) -> Vec<NativeResponsePolicy> {
    let mut policies = crate::response_policy::for_methods(methods);
    for policy in &mut policies {
        if policy.service == "Ice.BO.DynamicQuerySvc" {
            // Menu sharing is not Company=current-or-blank. The query filters
            // visibility and the receiver verifies each row against that scope.
            if let Some(tables) = &mut policy.tables {
                tables.unscoped_tables.push("Results".into());
            }
        }
    }
    policies
}
fn failure(message: &str) -> GuestReply {
    crate::failure("MENU_EXPORT_INVALID", message)
}
fn literal(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}
fn guid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
fn action(
    mut request: GuestRequest,
    phase: &str,
    service: &str,
    method: &str,
    args: Value,
    label: &str,
) -> GuestReply {
    request.state["phase"] = json!(phase);
    request.state["_progress"] = json!({"request":label,"response":"Menu batch received; building XML and system snapshots"});
    crate::native(service, method, args, false, request.state)
}

pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation!="acquire" {if let Err(diagnostic)=prefix(&request) {return GuestReply::Failure {diagnostic};}}
    if request.operation == "materialize" {
        return materialize(request);
    }
    if !matches!(request.operation.as_str(), "discover" | "acquire") {
        return failure("Menus support discovery and exact-identity collection.");
    }
    if request.state["phase"]=="ancestors" {return receive_dependencies(request);}
    if let Some(error) = request.error.take() {
        return GuestReply::Failure { diagnostic: error };
    }
    if !request.state.is_object() {
        request.state = json!({});
    }
    match request.state["phase"].as_str() {
        Some("session") => {
            let p = &request.response.as_ref().unwrap_or(&Value::Null)["parameters"];
            if p["companyID"] != request.company || !p["tenantID"].is_string() {
                return failure("The session did not confirm the selected company and tenant.");
            }
            request.state["tenant"] = p["tenantID"].clone();
        }
        Some("metadata") => {
            let fields = request
                .response
                .as_ref()
                .and_then(|v| v["returnObj"]["TableFieldList"].as_array());
            let Some(fields) = fields else {
                return failure("The menu field dictionary is missing.");
            };
            if ![
                "Company",
                "MenuID",
                "ParentMenuID",
                "CompanyVisibility",
                "SystemFlag",
                "SysRowID",
                "SysRevID",
            ]
            .iter()
            .all(|name| fields.iter().any(|f| f["FieldName"] == *name))
            {
                return failure("The menu dictionary omitted a required field.");
            }
            request.state["fields"] = json!(fields);
        }
        Some("companies") => {
            if request.response.as_ref().is_some_and(|v| {
                v["returnObj"]["Errors"]
                    .as_array()
                    .is_some_and(|r| !r.is_empty())
            }) {
                return failure("The tenant company query reported an error.");
            }
            let Some(rows) = request
                .response
                .as_ref()
                .and_then(|v| v["returnObj"]["Results"].as_array())
            else {
                return failure("The tenant company read omitted its rows.");
            };
            let size = request.page_size.unwrap_or(4000);
            if size == 0 || size > i32::MAX as u32 {
                return failure("Page size must be a whole number from 1 to 2147483647.");
            }
            if rows.len() > size as usize {
                return failure("The tenant company response exceeded its requested page size.");
            }
            let mut companies = request.state["companies"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let Some(mut seen) = companies.iter().map(|value| value.as_str()
                .filter(|id| !id.is_empty()).map(str::to_owned)).collect::<Option<BTreeSet<_>>>() else {
                return failure("The tenant company saved progress has an invalid identity.");
            };
            if seen.len() != companies.len() {
                return failure("The tenant company saved progress repeats an identity.");
            }
            for row in rows {
                let Some(id) = row["Company"].as_str().filter(|id| !id.is_empty()) else {
                    return failure("A tenant company has no complete text identity.");
                };
                if request.state["companyAfter"].as_str() == Some(id) || !seen.insert(id.to_owned()) {
                    return failure("The tenant company page repeated an identity or did not advance.");
                }
            }
            // Validate the complete page before extending the retained tenant scope.
            companies.extend(rows.iter().map(|row| row["Company"].clone()));
            request.state["companies"] = json!(companies);
            if rows.len() == size as usize {
                request.state["companyAfter"] = rows.last().unwrap()["Company"].clone();
                return company_action(request);
            }
            request.state["companiesDone"] = json!(true);
        }
        Some("page") => return receive(request),
        _ => {}
    }
    request.response = None;
    if !request.state["tenant"].is_string() {
        return action(
            request,
            "session",
            "Ice.Lib.SessionModSvc",
            "GetCurrentValuesFromServer",
            json!({}),
            "Checking menu company and tenant scope",
        );
    }
    if !request.state["fields"].is_array() {
        return action(
            request,
            "metadata",
            "Ice.BO.BAQDesignerSvc",
            "GetFieldList",
            json!({"dataTableID":"Menu","schema":"Ice"}),
            "Reading the menu field contract",
        );
    }
    if request.state["tenant"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        && request.state["companiesDone"] != true
    {
        return company_action(request);
    }
    let arguments = match query(&request, false) {
        Ok(v) => v,
        Err(e) => return failure(&e),
    };
    action(
        request,
        "page",
        "Ice.BO.DynamicQuerySvc",
        "Execute",
        arguments,
        "Reading complete menu definitions and system state",
    )
}
fn company_action(request: GuestRequest) -> GuestReply {
    let args = match query(&request, true) {
        Ok(v) => v,
        Err(e) => return failure(&e),
    };
    action(
        request,
        "companies",
        "Ice.BO.DynamicQuerySvc",
        "Execute",
        args,
        "Reading companies in the current tenant",
    )
}

/// Execute an unsaved BAQ. Explicit scope precedes keyset paging. No saved
/// query, helper, menu or permission is created by collection.
fn query(request: &GuestRequest, companies: bool) -> Result<Value, String> {
    let size = request.page_size.unwrap_or(4000);
    if size == 0 || size > i32::MAX as u32 {
        return Err("Page size must be a whole number from 1 to 2147483647.".into());
    }
    let table = if companies { "SysCompany" } else { "Menu" };
    let row = |extra: Value| {
        let mut r = json!({"Company":request.company,"QueryID":"HerbertMenuRead","SubQueryID":"00000000-0000-0000-0000-000000000001","RowMod":"A"});
        r.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        r
    };
    let mut ds = json!({"DynamicQuery":[{"Company":request.company,"QueryID":"HerbertMenuRead","Description":"Unsaved menu backup read","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],
        "QuerySubQuery":[row(json!({"Name":"TopLevel","Type":"TopLevel","Seq":1,"OrderByOffSet":"0","OrderByFetch":size.to_string()}))],
        "QueryTable":[row(json!({"TableID":"M","DBSchemaName":"Ice","DBTableName":table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","DBSchemaName":"","DBTableName":"","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))],
        "QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    let company_fields = json!([{"FieldName":"Company","DataType":"nvarchar"}]);
    let fields = if companies {
        &company_fields
    } else {
        &request.state["fields"]
    };
    for (i, f) in fields
        .as_array()
        .ok_or("Missing menu fields.")?
        .iter()
        .enumerate()
    {
        let name = f["FieldName"].as_str().ok_or("Missing menu field name.")?;
        let ty = f["DataType"].as_str().ok_or("Missing menu field type.")?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err("Invalid menu dictionary name.".into());
        }
        let formula = match ty {
            "timestamp" => format!("CONVERT(bigint,M.{name})"),
            "nvarchar" | "xml" => format!("CONVERT(nvarchar(max),M.{name})"),
            _ => format!("M.{name}"),
        };
        ds["QueryField"].as_array_mut().unwrap().push(row(json!({"TableID":"Calculated","FieldName":name,"Alias":name,"DBSchemaName":"","DBTableName":"","DBFieldName":"","DataType":if ty=="timestamp" {"bigint"} else if ty=="xml" {"nvarchar"} else {ty},"FieldFormat":"","FieldLabel":name,"Seq":i+1,"IsCalculated":true,"Formula":formula,"Updatable":false})));
    }
    let mut add = |field: &str, op: &str, value: String, logic: &str, left: &str, right: &str| {
        let seq = ds["QueryWhereItem"].as_array().unwrap().len() + 1;
        ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0000-0000-{seq:012}"),"TableID":"M","FieldName":field,"CompOp":op,"RValue":value,"IsConst":true,"CriteriaType":2,"Seq":seq,"AndOr":if seq==1 {""} else {logic},"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
    };
    if companies {
        add(
            "TenantID",
            "=",
            literal(request.state["tenant"].as_str().ok_or("Missing tenant.")?),
            "And",
            "",
            "",
        );
        if let Some(after) = request.state["companyAfter"].as_str() {
            add("Company", ">", literal(after), "And", "", "");
        }
    } else {
        // Blank-company legacy rows remain explicit even when visibility is 0.
        add("Company", "=", literal(&request.company), "And", "(", "");
        add("Company", "=", literal(""), "Or", "", "");
        let peers = request.state["companies"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        add(
            "CompanyVisibility",
            "=",
            "10".into(),
            "Or",
            "",
            if peers.is_empty() { ")" } else { "" },
        );
        if !peers.is_empty() {
            add("CompanyVisibility", "=", "5".into(), "Or", "(", "");
            for (i, peer) in peers.iter().enumerate() {
                add(
                    "Company",
                    "=",
                    literal(peer.as_str().ok_or("Invalid tenant company.")?),
                    if i == 0 { "And" } else { "Or" },
                    if i == 0 { "(" } else { "" },
                    if i + 1 == peers.len() { ")))" } else { "" },
                );
            }
        }
        if let Some(cursor) = request.cursor.as_deref() {
            let c: Value = serde_json::from_str(cursor).map_err(|_| "Invalid menu cursor.")?;
            let after = c["after"]
                .as_str()
                .filter(|s| guid(s))
                .ok_or("Invalid menu row cursor.")?;
            if c["menuVersion"] != 1 {
                return Err("Restart menu discovery to use the current cursor.".into());
            }
            add("SysRowID", ">", literal(after), "And", "", "");
        }
        if let Some(id) = &request.identity {
            if id.category != "menus-and-launch-definitions" || id.kind != "PersistedMenu" || id.keys.len() != 2 {
                return Err("Select an exact menu identity.".into());
            }
            for field in ["Company", "MenuID"] {
                add(
                    field,
                    "=",
                    literal(id.keys.get(field).ok_or("Incomplete menu identity.")?),
                    "And",
                    "",
                    "",
                );
            }
        }
    }
    ds["QuerySortBy"] = json!([row(
        json!({"TableID":"M","FieldName":if companies{"Company"}else{"SysRowID"},"Seq":1,"IsAsc":true})
    )]);
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}

fn identity(company: &str, kind: &str, keys: BTreeMap<String, String>) -> Identity {
    Identity {
        category: "menus-and-launch-definitions".into(),
        kind: kind.into(),
        scope: if company.is_empty() {
            Scope::Global
        } else {
            Scope::Company {
                company: company.into(),
            }
        },
        keys,
    }
}
fn menu_identity(row: &Value, selected: &str) -> Result<Identity, String> {
    let company = row["Company"]
        .as_str()
        .ok_or("A menu has no company key.")?;
    let id = row["MenuID"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("A menu has no ID.")?;
    let mut result = identity(
        company,
        "PersistedMenu",
        BTreeMap::from([
            ("Company".into(), company.into()),
            ("MenuID".into(), id.into()),
        ]),
    );
    if !company.is_empty() && company != selected {
        result.scope = Scope::SharedCompany {
            company: selected.into(),
            owner_company: company.into(),
        };
    }
    Ok(result)
}
fn dependency_closure(graph: &[Value], company: &str, prefix: &str) -> (Vec<Value>,Vec<Value>,Vec<Value>) {
    let roots: BTreeSet<_>=graph.iter().enumerate().filter(|(_,row)|row["MenuID"].as_str().unwrap().starts_with(prefix)).map(|(index,_)|index).collect();
    let mut included=roots.clone();let mut dependencies=BTreeSet::new();
    // Rows by MenuID in graph order, built once instead of scanning the graph per ancestor step.
    let mut by_id:BTreeMap<&str,Vec<usize>>=BTreeMap::new();
    for (index,row) in graph.iter().enumerate() {if let Some(id)=row["MenuID"].as_str() {by_id.entry(id).or_default().push(index);}}
    for root in &roots {
        let mut node=*root;let mut path=BTreeSet::new();
        loop {
            included.insert(node);
            if !path.insert(node) || graph[node]["SystemFlag"]==true {break;}
            // Role membership includes an ancestor that is also another
            // selected root. Body retrieval is decided separately below.
            if node!=*root {dependencies.insert(node);}
            let parent=graph[node]["ParentMenuID"].as_str().unwrap();if parent.is_empty() {break;}
            let mut candidates=by_id.get(parent).cloned().unwrap_or_default();
            let current=candidates.iter().any(|index|graph[*index]["Company"]==company);
            candidates.retain(|index|!current || graph[*index]["Company"]!="");
            included.extend(candidates.iter().copied());
            if candidates.len()!=1 {break;}
            node=candidates[0];
        }
    }
    (included.into_iter().map(|index|graph[index].clone()).collect(),dependencies.into_iter().map(|index|graph[index].clone()).collect(),roots.into_iter().map(|index|graph[index].clone()).collect())
}
fn prepare_received(mut request: GuestRequest, state: Value, next: Option<String>) -> GuestReply {
    let state=with_settings(state,&request.state);
    if request.state["_hostPreparePage"]==true {
        GuestReply::PreparedPage {state,next_cursor:next,next_state:request.state}
    } else {request.state=state;materialize(request)}
}
fn dependency_action(mut request: GuestRequest) -> GuestReply {
    let mut query_request=request.clone();query_request.identity=None;
    query_request.cursor=request.state["dependencyAfter"].as_str().map(|after|json!({"menuVersion":1,"after":after}).to_string());
    let mut args=match query(&query_request,false) {Ok(args)=>args,Err(message)=>return failure(&message)};
    let witnesses:Vec<_>=request.state["requiredDependencies"].as_array().unwrap().iter().filter(|witness|!request.state["dependencyBodies"].as_array().unwrap().iter().any(|body|row_key(body)==row_key(witness))).collect();
    if witnesses.is_empty() {return finish_dependencies(request);}
    for (index,witness) in witnesses.iter().enumerate() {
        for (part,field) in ["Company","MenuID"].iter().enumerate() {
            let seq=args["queryDS"]["QueryWhereItem"].as_array().unwrap().len()+1;
            args["queryDS"]["QueryWhereItem"].as_array_mut().unwrap().push(json!({"Company":request.company,"QueryID":"HerbertMenuRead","SubQueryID":"00000000-0000-0000-0000-000000000001","RowMod":"A","CriteriaID":format!("00000000-0000-0000-0000-{seq:012}"),"TableID":"M","FieldName":field,"CompOp":"=","RValue":literal(witness[*field].as_str().unwrap()),"IsConst":true,"CriteriaType":2,"Seq":seq,"AndOr":if part==0 && index>0 {"Or"}else{"And"},"Neg":false,"LeftP":if part==0 && index==0 {"(("}else if part==0 {"("}else{""},"RightP":if part==1 && index+1==witnesses.len() {"))"}else if part==1 {")"}else{""},"UseHaving":false}));
        }
    }
    request.response=None;
    action(request,"ancestors","Ice.BO.DynamicQuerySvc","Execute",args,"Reading complete required custom menu ancestors")
}
fn finish_dependencies(mut request: GuestRequest) -> GuestReply {
    let witnesses=request.state["requiredDependencies"].as_array().unwrap();
    let bodies=request.state["dependencyBodies"].as_array().unwrap();
    let retained:BTreeSet<_>=bodies.iter().map(row_key).collect();
    let missing:Vec<_>=witnesses.iter().filter(|witness|!retained.contains(&row_key(witness))).cloned().collect();
    let mut errors:Vec<Diagnostic>=serde_json::from_value(request.state["dependencyErrors"].clone()).unwrap();
    for witness in &missing {
        let mut error=Diagnostic::new("MENU_ANCESTOR_MISSING","A required authorized custom ancestor body was not retained. The selected closure is incomplete.");error.identity=menu_identity(witness,&request.company).ok();errors.push(error);
    }
    let mut prepared=request.state["finalPage"].clone();
    prepared["dependencyBodiesComplete"]=json!(missing.is_empty() && errors.is_empty() && prepared["rootBodiesComplete"]==true);prepared["missingDependencyBodies"]=json!(missing);
    prepared["rows"].as_array_mut().unwrap().extend(bodies.iter().cloned());
    prepared["_selectionDiagnostics"].as_array_mut().unwrap().extend(errors.into_iter().map(|error|json!(error)));
    request.state["phase"]=Value::Null;
    prepare_received(request,prepared,None)
}
fn receive_dependencies(mut request: GuestRequest) -> GuestReply {
    if let Some(mut error)=request.error.take() {
        // A denied body read blocks the ancestor closure, not already useful roots.
        error.code="MENU_ANCESTOR_READ_FAILED".into();
        request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(error));return finish_dependencies(request);
    }
    let result=&request.response.as_ref().unwrap_or(&Value::Null)["returnObj"];
    let Some(rows)=result["Results"].as_array().filter(|_|!result["Errors"].as_array().is_some_and(|errors|!errors.is_empty())) else {
        request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(Diagnostic::new("MENU_ANCESTOR_READ_INVALID","The required ancestor read did not return a valid Epicor page.")));return finish_dependencies(request);
    };
    let size=request.page_size.unwrap_or(4000) as usize;
    if rows.len()>size {request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(Diagnostic::new("MENU_ANCESTOR_READ_INVALID","The ancestor page exceeded its Epicor requested page size.")));return finish_dependencies(request);}
    for row in rows {
        let identity=match menu_identity(row,&request.company) {Ok(id)=>id,Err(_)=>{request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(Diagnostic::new("MENU_ANCESTOR_READ_INVALID","An ancestor body has an incomplete Epicor ID.")));continue;}};
        let witness=request.state["requiredDependencies"].as_array().unwrap().iter().find(|witness|row_key(witness)==row_key(row));
        let issue=if !scoped_row(row,&request) {Some(("MENU_ANCESTOR_UNAUTHORIZED","An ancestor body is outside the authorized company/tenant scope."))}
            else if witness.is_none() {Some(("MENU_ANCESTOR_UNEXPECTED","The ancestor read returned an unrequested physical menu."))}
            else if !same_witness(row,witness.unwrap()) {Some(("MENU_ANCESTOR_CHANGED","A required ancestor changed after its graph witness. Its body is not part of a complete closure."))}
            else if request.state["dependencyBodies"].as_array().unwrap().iter().any(|body|row_key(body)==row_key(row)) {Some(("MENU_ANCESTOR_DUPLICATE","The ancestor read repeated a physical menu body."))}
            else {None};
        if let Some((code,message))=issue {let mut error=Diagnostic::new(code,message);error.identity=Some(identity);request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(error));continue;}
        let errors=source_errors(row,&request.company);request.state["dependencyErrors"].as_array_mut().unwrap().extend(errors.into_iter().map(|error|json!(error)));
        request.state["dependencyBodies"].as_array_mut().unwrap().push(row.clone());
    }
    if rows.len()==size {
        let after=rows.last().and_then(|row|row["SysRowID"].as_str()).filter(|value|guid(value));
        if after.is_none() || request.state["dependencyAfter"].as_str()==after {
            request.state["dependencyErrors"].as_array_mut().unwrap().push(json!(Diagnostic::new("MENU_ANCESTOR_READ_INVALID","The ancestor page did not advance a valid Epicor saved position.")));return finish_dependencies(request);
        }
        request.state["dependencyAfter"]=json!(after);return dependency_action(request);
    }
    finish_dependencies(request)
}
fn receive(mut request: GuestRequest) -> GuestReply {
    let result = &request.response.as_ref().unwrap_or(&Value::Null)["returnObj"];
    if result["Errors"].as_array().is_some_and(|r| !r.is_empty()) {
        return failure(&format!("The menu query reported: {}", result["Errors"]));
    }
    let Some(rows) = result["Results"].as_array() else {
        return failure("The menu query omitted its rows.");
    };
    if rows.len() > request.page_size.unwrap_or(4000) as usize {
        return failure("The menu response exceeded its requested page size.");
    }
    let mut graph = request.state["graph"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut seen: BTreeSet<_> = graph
        .iter()
        .filter_map(|r| menu_identity(r, &request.company).ok())
        .map(|id| id.keys)
        .collect();
    let mut page_errors=Vec::new();
    for row in rows {
        let id = match menu_identity(row, &request.company) {
            Ok(v) => v,
            Err(e) => return failure(&e),
        };
        if !seen.insert(id.keys) {
            return failure("The menu collection repeated an identity.");
        }
        if !row["SystemFlag"].is_boolean()
            || !row["ParentMenuID"].is_string()
            || !row["CompanyVisibility"].is_number()
        {
            return failure("A menu omitted its parent, system flag or visibility.");
        }
        if !row["SysRowID"].as_str().is_some_and(guid) || !valid_revision(&row["SysRevID"]) {
            return failure("A menu omitted its exact Epicor row ID or lossless revision check.");
        }
        let owner = row["Company"].as_str().unwrap();
        let visible = owner == request.company
            || owner.is_empty()
            || row["CompanyVisibility"] == 10
            || (row["CompanyVisibility"] == 5
                && request.state["companies"]
                    .as_array()
                    .is_some_and(|peers| peers.iter().any(|p| p == owner)));
        if !visible {
            return failure(
                "The query returned a menu outside the selected company and tenant scope.",
            );
        }
        // Every row must reach the ancestor graph (a selected menu's parents need not
        // match the prefix), so the prefix is not sent to the server. Only selected
        // rows are validated here; required ancestors are validated when their
        // bodies are read. Unselected rows produce no diagnostics.
        let selected=request.operation=="acquire" || prefix(&request).map_or(true,|value|row["MenuID"].as_str().unwrap().starts_with(&value));
        let validated=if selected {let errors=source_errors(row,&request.company);let valid=errors.is_empty();page_errors.extend(errors);valid}else{false};
        graph.push(json!({"Company":owner,"MenuID":row["MenuID"],"ParentMenuID":row["ParentMenuID"],"SystemFlag":row["SystemFlag"],"CompanyVisibility":row["CompanyVisibility"],"SysRowID":row["SysRowID"],"SysRevID":row["SysRevID"],"bodyValidated":validated}));
    }
    let next = if rows.len() == request.page_size.unwrap_or(4000) as usize {
        let Some(after) = rows
            .last()
            .and_then(|r| r["SysRowID"].as_str())
            .filter(|s| guid(s))
        else {
            return failure("The menu page has no final row ID.");
        };
        let next = json!({"menuVersion":1,"after":after}).to_string();
        if request.cursor.as_ref() == Some(&next) {
            return failure("The menu page did not advance.");
        }
        Some(next)
    } else {
        None
    };
    let page = request.state["page"].as_u64().unwrap_or(1);
    let selection=if request.operation=="acquire" {String::new()}else {match prefix(&request) {Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}};
    let selected:Vec<_>=rows.iter().filter(|row|row["MenuID"].as_str().unwrap().starts_with(&selection)).cloned().collect();
    let mut state=json!({"rows":selected,"graph":if next.is_none(){json!(graph)}else{Value::Null},"page":page,"next":next,"_selectionDiagnostics":page_errors});
    if !selection.is_empty() && request.operation!="acquire" {
        // Roles become known only after later pages reveal child references.
        // Keep selected full bodies in the existing state/queue path until
        // graph completion, then emit each physical artifact once with all
        // roles. Retained roots never need a body reread for role discovery.
        let mut retained=request.state["selectedBodies"].as_array().cloned().unwrap_or_default();retained.extend(selected);
        state["rows"]=if next.is_none() {json!(retained)}else{json!([])};request.state["selectedBodies"]=json!(retained);
    }
    request.state["graph"] = json!(graph);
    request.state["page"] = json!(page + 1);
    request.state["phase"] = Value::Null;
    if next.is_none() && !selection.is_empty() && request.operation!="acquire" {
        let (closure,ancestor_roles,roots)=dependency_closure(&graph,&request.company,&selection);
        let root_keys:BTreeSet<_>=roots.iter().map(row_key).collect();
        let dependencies:Vec<_>=ancestor_roles.iter().filter(|row|!root_keys.contains(&row_key(row))).cloned().collect();
        state["graph"]=if roots.is_empty() {Value::Null}else{json!(closure)};
        state["selectionRoots"]=json!(roots.iter().map(|row|menu_identity(row,&request.company).unwrap()).collect::<Vec<_>>());
        state["rootBodiesComplete"]=json!(roots.iter().all(|row|row["bodyValidated"]==true));
        state["dependencyKeys"]=json!(ancestor_roles.iter().map(row_key).collect::<Vec<_>>());
        state["dependencyBodiesComplete"]=json!(dependencies.is_empty() && state["rootBodiesComplete"]==true);
        if !dependencies.is_empty() {
            let available:Vec<_>=rows.iter().filter(|row|dependencies.iter().any(|witness|row_key(row)==row_key(witness) && same_witness(row,witness))).cloned().collect();
            let errors:Vec<_>=available.iter().flat_map(|row|source_errors(row,&request.company)).collect();
            request.state["finalPage"]=state;request.state["requiredDependencies"]=json!(dependencies);request.state["dependencyBodies"]=json!(available);request.state["dependencyErrors"]=json!(errors);request.state["dependencyAfter"]=Value::Null;
            return dependency_action(request);
        }
    }
    prepare_received(request,state,next)
}

fn materialize(request: GuestRequest) -> GuestReply {
    let Some(rows) = request.state["rows"].as_array() else {
        return failure("The prepared menu page has no rows.");
    };
    let mut records = vec![];
    let mut diagnostics:Vec<Diagnostic>=request.state.get("_selectionDiagnostics").map(|value|serde_json::from_value(value.clone()).unwrap_or_default()).unwrap_or_default();
    let mut stock = vec![];
    for row in rows {
        if !matches!(row["CompanyVisibility"].as_i64(), Some(0 | 5 | 10)) {
            let mut d = Diagnostic::new(
                "MENU_VISIBILITY_UNKNOWN",
                "The menu has an unknown company visibility. Its source is preserved; review its restore scope.",
            );
            d.identity = menu_identity(row, &request.company).ok();
            if !diagnostics.iter().any(|existing|existing.code==d.code && existing.identity==d.identity) {diagnostics.push(d);}
        }
        if row["SystemFlag"] == true && request.operation != "acquire" {
            stock.push(row.clone());
            continue;
        }
        let id = match menu_identity(row, &request.company) {
            Ok(id) => id,
            Err(e) => return failure(&e),
        };
        let group = if row["SystemFlag"] == true {
            "System-menu snapshot".to_owned()
        } else {
            format!("Custom menus - {}", if id.keys["Company"].is_empty() {"Shared"} else {&id.keys["Company"]})
        };
        let mut definition = json!({"Menu":[native_row(row)],"source":row,"herbertOutputGroup":group});
        let ancestor=request.state["dependencyKeys"].as_array().is_some_and(|keys|keys.contains(&json!(row_key(row))));
        let root=request.state["_pluginSettings"]["menuIdPrefix"].as_str().is_some_and(|value|!value.is_empty() && row["MenuID"].as_str().unwrap().starts_with(value));
        if ancestor || root {
            let mut roles=Vec::new();if root {roles.push("prefixMatchedRoot");}if ancestor {roles.push("requiredCustomAncestor");}
            definition["herbertSelectionRoles"]=json!(roles);definition["herbertSelectionRole"]=json!(if root {"prefixMatchedRoot"}else{"requiredCustomAncestor"});
        }
        match append(
            &mut definition,
            &format!("{}.xml", crate::bpm_code::label(&id.keys["MenuID"])),
        ) {
            Ok(()) => {}
            Err(mut d) => {
                d.identity = Some(id.clone());
                diagnostics.push(d);
            }
        }
        records.push((id, definition));
    }
    if !stock.is_empty() {
        let page = request.state["page"].as_u64().unwrap_or(1);
        let id = identity(
            &request.company,
            "SystemMenuSnapshot",
            BTreeMap::from([("Page".into(), format!("{page:06}"))]),
        );
        let mut definition = json!({"Menu":stock.iter().map(native_row).collect::<Vec<_>>(),"source":stock,"herbertOutputGroup":"System-menu snapshot","purpose":"Captured system state, not detected customer changes. Multi-row snapshot: do not import it as one MenuHandler item. Restore only reviewed settings to existing system identities."});
        if let Err(mut d) = append(&mut definition, &format!("SystemMenus-{page:06}.xml")) {
            d.identity = Some(id.clone());
            diagnostics.push(d);
        }
        records.push((id, definition));
    }
    if let Some(graph) = request.state["graph"]
        .as_array()
        .filter(|_| request.operation != "acquire")
    {
        let (mut definition, issues) = restore_graph(graph, &request.company);
        if let Some(roots)=request.state.get("selectionRoots") {
            definition["selectionRoots"]=roots.clone();definition["requiredCustomAncestors"]=request.state["dependencyKeys"].clone();definition["dependencyBodiesComplete"]=request.state["dependencyBodiesComplete"].clone();definition["missingDependencyBodies"]=request.state["missingDependencyBodies"].clone();
            let mut retained:BTreeSet<_>=serde_json::from_value::<Vec<Identity>>(roots.clone()).unwrap().into_iter().map(|id|(id.keys["Company"].clone(),id.keys["MenuID"].clone())).collect();
            retained.extend(serde_json::from_value::<Vec<(String,String)>>(request.state["dependencyKeys"].clone()).unwrap());
            definition["customRestoreOrder"].as_array_mut().unwrap().retain(|id|retained.contains(&(id["keys"]["Company"].as_str().unwrap().into(),id["keys"]["MenuID"].as_str().unwrap().into())));
            if request.state["dependencyBodiesComplete"]!=true {definition["restoreOrderComplete"]=json!(false);}
        }
        diagnostics.extend(issues);
        records.push((
            identity(
                &request.company,
                "MenuRestoreIndex",
                BTreeMap::from([("Name".into(), "Restore order".into())]),
            ),
            definition,
        ));
    }
    if request.operation == "acquire" {
        let Some(id) = request.identity else {
            return failure("Select an exact menu identity.");
        };
        let Some((_, definition)) = records.into_iter().find(|(found, _)| found == &id) else {
            return failure("The exact menu is absent from the authorized collection.");
        };
        return GuestReply::Definition {
            identity: id,
            format: "menu-native-xml".into(),
            definition,
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        };
    }
    GuestReply::InventoryData {
        inventory: Box::new(GuestReply::Inventory {
            identities: records.iter().map(|(id, _)| id.clone()).collect(),
            next_cursor: request.state["next"].as_str().map(str::to_owned),
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
        records,
    }
}

/// Native SQL supplies transient defaults. Preserve authored strings; Alias
/// alone uses the native line representation, which Update converts back to ~.
fn native_row(source: &Value) -> Value {
    let mut row = source.clone();
    row.as_object_mut().unwrap().remove("CompanyVisibility");
    for name in [
        "Dashboard",
        "Extension",
        "Options",
        "Customization",
        "CustomizationKinetic",
        "FormName",
        "ProgramKinetic",
        "CustomizationType",
        "RowMod",
    ] {
        row[name] = json!("");
    }
    for name in [
        "DeveloperMode",
        "ReadOnly",
        "ShowWebNavBar",
        "ValidateBeforeLaunch",
        "Kinetic",
        "Select",
        "IsDuplicate",
    ] {
        row[name] = json!(false);
    }
    row["CanModify"] = json!(true);
    row["BitFlag"] = json!(0);
    row["AllCompanies"] = json!(source["CompanyVisibility"] != 0);
    if let Some(alias) = source["Alias"].as_str() {
        row["Alias"] = json!(alias.replace('~', "\n"));
    }
    row
}
fn append(definition: &mut Value, name: &str) -> Result<(), Diagnostic> {
    let schema: Vec<(String, String)> =
        serde_json::from_str(include_str!("menu-fields.json")).unwrap();
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><MenuDataSet xmlns=\"http://www.epicor.com/Ice/300/BO/Menu/Menu\">",
    );
    for row in definition["Menu"].as_array().unwrap() {
        text.push_str("<Menu>");
        for (field, kind) in &schema {
            let Some(value) = row.get(field).filter(|v| !v.is_null()) else {
                continue;
            };
            let value = match (kind.as_str(), value) {
                ("string" | "Guid", Value::String(s)) => s.clone(),
                ("bool", Value::Bool(b)) => b.to_string(),
                ("int" | "long", Value::Number(n)) => n.to_string(),
                ("long", Value::String(s)) if s.parse::<i64>().is_ok() => s.clone(),
                _ => {
                    return Err(Diagnostic::new(
                        "MENU_XML_INVALID",
                        &format!("Menu.{field} does not match its Epicor {kind} type."),
                    ));
                }
            };
            text.push_str(&format!(
                "<{field}>{}</{field}>",
                crate::classic_archive::escape(&value)?
            ));
        }
        text.push_str("</Menu>");
    }
    text.push_str("</MenuDataSet>");
    definition["herbertDirectOutput"] = json!(true);
    definition["herbertSources"] = json!({"files":[{"name":name,"text":text}]});
    Ok(())
}

fn restore_graph(rows: &[Value], company: &str) -> (Value, Vec<Diagnostic>) {
    let mut by_id: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        by_id
            .entry(row["MenuID"].as_str().unwrap())
            .or_default()
            .push(i);
    }
    let mut edges = BTreeMap::new();
    let mut issues = vec![];
    let mut links = vec![];
    for (i, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r["SystemFlag"] == false)
    {
        let parent = row["ParentMenuID"].as_str().unwrap();
        if parent.is_empty() {
            continue;
        }
        let candidates = by_id.get(parent).cloned().unwrap_or_default();
        // The runtime explicitly prefers the selected-company row over a
        // blank-company variant. Other shared conflicts remain ambiguous.
        let has_current = candidates.iter().any(|n| rows[*n]["Company"] == company);
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|n| !has_current || rows[*n]["Company"] != "")
            .collect();
        links.push(json!({"menu":menu_identity(row,company).unwrap(),"parentCandidates":candidates.iter().map(|n|menu_identity(&rows[*n],company).unwrap()).collect::<Vec<_>>() }));
        if candidates.len() == 1 {
            edges.insert(i, candidates[0]);
        } else {
            let mut d = Diagnostic::new(
                if candidates.is_empty() {
                    "MENU_PARENT_MISSING"
                } else {
                    "MENU_PARENT_AMBIGUOUS"
                },
                &format!(
                    "Menu {} has {} applicable parents named {parent}. Its source is preserved; resolve the parent before restore.",
                    row["MenuID"].as_str().unwrap(),
                    candidates.len()
                ),
            );
            d.identity = menu_identity(row, company).ok();
            issues.push(d);
        }
    }
    let mut order = vec![];
    let mut done = BTreeSet::new();
    let mut blocked = BTreeSet::new();
    for (i, row) in rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r["SystemFlag"] == false)
    {
        let mut chain = vec![];
        let mut path = BTreeSet::new();
        let mut node = i;
        let valid = loop {
            if !path.insert(node) {
                let mut d = Diagnostic::new(
                    "MENU_PARENT_CYCLE",
                    "The menu parent chain contains a cycle. Its source is preserved; correct the chain before restore.",
                );
                d.identity = menu_identity(row, company).ok();
                issues.push(d);
                break false;
            }
            if blocked.contains(&node) {
                break false;
            }
            if rows[node]["SystemFlag"] == true || done.contains(&node) {
                break true;
            }
            chain.push(node);
            if let Some(parent) = edges.get(&node) {
                node = *parent;
            } else {
                break rows[node]["ParentMenuID"] == "";
            }
        };
        if !valid {
            blocked.extend(chain);
            continue;
        }
        for node in chain.into_iter().rev() {
            if done.insert(node) {
                order.push(menu_identity(&rows[node], company).unwrap());
            }
        }
    }
    (
        json!({"herbertOutputGroup":"Restore guidance","menus":rows,"parentLinks":links,"customRestoreOrder":order,"restoreOrderComplete":blocked.is_empty(),"blockedMenus":blocked.into_iter().map(|n|menu_identity(&rows[n],company).unwrap()).collect::<Vec<_>>(),"instructions":"Restore custom parents before children. System menus are existing target dependencies. Review unresolved links and system settings. CompanyVisibility is retained in source: native AllCompanies restores sharing under the target tenant. Referenced security assignments, apps and reports are separate collections."}),
        issues,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "menus-and-launch-definitions".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: json!({"tenant":"T","companies":["C","Peer"],"fields":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}],"_hostPreparePage":true}),
            response: None,
            error: None,
        }
    }
    fn menu(company: &str, id: &str, parent: &str, system: bool, visibility: i32) -> Value {
        json!({"Company":company,"MenuID":id,"ParentMenuID":parent,"SystemFlag":system,"CompanyVisibility":visibility,"SysRowID":if id=="A"{"00000000-0000-0000-0000-000000000001"}else{"00000000-0000-0000-0000-000000000002"},"MenuDesc":"Ω & < >","Alias":"one~two","Enabled":false,"DoNotDisplayInMenu":true,"Arguments":"-ro=true","SysRevID":1})
    }
    fn fixture(company: &str,id: &str,parent: &str,system: bool,visibility: i32,index: u64) -> Value {
        let mut row=menu(company,id,parent,system,visibility);row["SysRowID"]=json!(format!("00000000-0000-0000-0000-{index:012}"));row["SysRevID"]=json!(9007199254740993u64+index);row
    }
    fn selected_run(rows: Vec<Value>, bodies: Vec<Value>, prefix: &str, size: u32) -> (Vec<(Identity,Value)>,Vec<Diagnostic>,Vec<Value>) {
        let mut r=request();r.page_size=Some(size);r.state["_pluginSettings"]=json!({"menuIdPrefix":prefix});r.state["phase"]=json!("page");
        let columns:BTreeMap<_,_>=rows.iter().flat_map(|row|row.as_object().unwrap().iter().map(|(name,value)|(name.clone(),if value.is_boolean() {"bit"}else if value.is_number() {"bigint"}else{"nvarchar"}))).collect();
        r.state["fields"]=json!(columns.into_iter().map(|(name,ty)|json!({"FieldName":name,"DataType":ty})).collect::<Vec<_>>());
        let mut raw_at=0;let mut output=Vec::new();let mut diagnostics=Vec::new();let mut calls=Vec::new();
        r.response=Some(json!({"returnObj":{"Results":rows.iter().take(size as usize).cloned().collect::<Vec<_>>()}}));raw_at+=size as usize;
        loop {
            match handle(r.clone()) {
                GuestReply::Action {action:HostAction::NativeRead {arguments,..},state} => {
                    assert_eq!(state["phase"],"ancestors");assert_eq!(state["_pluginSettings"]["menuIdPrefix"],prefix);calls.push(arguments);r.state=state;
                    let wanted=r.state["requiredDependencies"].as_array().unwrap();let existing=r.state["dependencyBodies"].as_array().unwrap();let after=r.state["dependencyAfter"].as_str();
                    let mut found:Vec<_>=bodies.iter().filter(|row|wanted.iter().any(|w|row_key(w)==row_key(row)) && !existing.iter().any(|old|row_key(old)==row_key(row)) && after.is_none_or(|after|row["SysRowID"].as_str().unwrap()>after)).cloned().collect();found.sort_by_key(|row|row["SysRowID"].as_str().unwrap().to_owned());found.truncate(size as usize);r.response=Some(json!({"returnObj":{"Results":found}}));
                },
                GuestReply::PreparedPage {state,next_cursor,next_state} => {
                    assert_eq!(state["_pluginSettings"]["menuIdPrefix"],prefix);let mut materialize_request=r.clone();materialize_request.operation="materialize".into();materialize_request.state=state;materialize_request.response=None;
                    let GuestReply::InventoryData {records,inventory}=handle(materialize_request)else{panic!()};output.extend(records);let GuestReply::Inventory {diagnostics:issues,..}=*inventory else{panic!()};diagnostics.extend(issues);
                    if next_cursor.is_none() {break;}
                    r.state=next_state;r.state["phase"]=json!("page");r.cursor=next_cursor;
                    r.response=Some(json!({"returnObj":{"Results":rows.iter().skip(raw_at).take(size as usize).cloned().collect::<Vec<_>>()}}));raw_at+=size as usize;
                },
                other=>panic!("Unexpected selected menu reply: {other:?}"),
            }
        }
        (output,diagnostics,calls)
    }
    fn restore_index(records: &[(Identity,Value)]) -> &Value {&records.iter().find(|(id,_)|id.kind=="MenuRestoreIndex").unwrap().1}
    #[test]
    fn unselected_rows_are_neither_validated_nor_diagnosed_and_the_query_keeps_every_row() {
        // "Other" has an unknown visibility; it is outside the prefix, so it is silent.
        let rows=vec![fixture("C","SelectedA","",false,0,1),fixture("C","Other","",false,7,2)];
        let (records,errors,_)=selected_run(rows.clone(),rows.clone(),"Selected",2);
        assert!(errors.is_empty(),"{errors:?}");
        assert_eq!(records.iter().filter(|(id,_)|id.kind=="PersistedMenu").count(),1);
        let (_,errors,_)=selected_run(rows.clone(),rows,"",2);
        assert!(errors.iter().any(|error|error.code=="MENU_VISIBILITY_UNKNOWN"));
        // The ancestor graph needs rows that do not match, so no server prefix filter.
        let mut r=request();r.state["_pluginSettings"]=json!({"menuIdPrefix":"Selected"});
        let text=query(&r,false).unwrap().to_string();
        assert!(!text.contains("Selected") && !text.to_uppercase().contains("LIKE"));
    }
    #[test]
    fn prefix_keeps_required_earlier_later_multilevel_ancestor_xml_and_deduplicates_roles() {
        let rows=vec![fixture("C","Parent","System",false,0,1),fixture("","System","",true,10,2),fixture("C","SelectedA","Parent",false,0,3),fixture("C","SelectedB","Grand",false,0,4),fixture("Peer","Grand","Parent",false,10,5),fixture("C","Other","",false,0,6)];
        let (actual,errors,calls)=selected_run(rows.clone(),rows.clone(),"Selected",2);assert!(errors.is_empty());assert!(!calls.is_empty());
        let menus:Vec<_>=actual.iter().filter(|(id,_)|id.kind=="PersistedMenu").collect();assert_eq!(menus.len(),4);assert_eq!(menus.iter().map(|(id,_)|id.keys.clone()).collect::<BTreeSet<_>>().len(),4);
        assert_eq!(menus.iter().filter(|(_,data)|data["herbertSelectionRole"]=="requiredCustomAncestor").count(),2);
        assert!(menus.iter().any(|(id,_)|matches!(&id.scope,Scope::SharedCompany {owner_company,..} if owner_company=="Peer")));
        for (id,data) in menus {
            let original=rows.iter().find(|row|row["Company"]==id.keys["Company"] && row["MenuID"]==id.keys["MenuID"]).unwrap();assert_eq!(&data["source"],original);
            let mut expected=json!({"Menu":[native_row(original)]});append(&mut expected,&format!("{}.xml",crate::bpm_code::label(&id.keys["MenuID"]))).unwrap();assert_eq!(data["herbertSources"],expected["herbertSources"]);
        }
        let guidance=restore_index(&actual);assert_eq!(guidance["dependencyBodiesComplete"],true);assert_eq!(guidance["restoreOrderComplete"],true);
        let order:Vec<_>=guidance["customRestoreOrder"].as_array().unwrap().iter().map(|id|id["keys"]["MenuID"].as_str().unwrap()).collect();assert!(order.iter().position(|id|*id=="Parent")<order.iter().position(|id|*id=="SelectedA"));assert!(order.iter().position(|id|*id=="Grand")<order.iter().position(|id|*id=="SelectedB"));
        assert!(!serde_json::to_string(guidance).unwrap().contains("Other"));assert!(guidance["menus"].as_array().unwrap().iter().all(|row|row["SysRevID"].as_u64().unwrap()>9007199254740992));
        let (both,errors,body_reads)=selected_run(vec![fixture("C","SelectedParent","",false,0,1),fixture("C","SelectedChild","SelectedParent",false,0,2)],vec![],"Selected",1);assert!(errors.is_empty());assert!(body_reads.is_empty());assert_eq!(both.iter().filter(|(id,_)|id.kind=="PersistedMenu").count(),2);assert_eq!(restore_index(&both)["requiredCustomAncestors"],json!([["C","SelectedParent"]]));
        let parent=&both.iter().find(|(id,_)|id.keys.get("MenuID").map(String::as_str)==Some("SelectedParent")).unwrap().1;assert_eq!(parent["herbertSelectionRoles"],json!(["prefixMatchedRoot","requiredCustomAncestor"]));
        let order=restore_index(&both)["customRestoreOrder"].as_array().unwrap();assert_eq!(order[0]["keys"]["MenuID"],"SelectedParent");assert_eq!(order[1]["keys"]["MenuID"],"SelectedChild");assert_eq!(restore_index(&both)["dependencyBodiesComplete"],true);
    }
    #[test]
    fn prefix_preserves_custom_system_variants_literal_empty_pages_zero_and_exact_independence() {
        let rows=vec![fixture("C","Beta","",false,0,1),fixture("C","Alpha","",false,0,2),fixture("Peer","Alpha","",false,10,3),fixture("","AlphaSystem","",true,10,4)];
        let (all,errors,_)=selected_run(rows.clone(),rows.clone(),"",2);assert!(errors.is_empty());assert_eq!(all.iter().filter(|(id,_)|id.kind=="PersistedMenu").count(),3);assert!(all.iter().filter(|(id,_)|id.kind=="PersistedMenu").all(|(_,data)|data.get("herbertSelectionRole").is_none()));
        let (records,errors,_)=selected_run(rows.clone(),rows.clone(),"Alpha",1);assert!(errors.is_empty());assert_eq!(records.iter().filter(|(id,_)|id.kind=="PersistedMenu").count(),2);let snapshot=&records.iter().find(|(id,_)|id.kind=="SystemMenuSnapshot").unwrap().1;assert_eq!(snapshot["source"].as_array().unwrap().len(),1);assert_eq!(snapshot["source"][0]["MenuID"],"AlphaSystem");
        for prefix in ["Missing","alpha"," Alpha","%"] {let (records,errors,calls)=selected_run(rows.clone(),rows.clone(),prefix,1);assert!(records.is_empty());assert!(errors.is_empty());assert!(calls.is_empty());}
        let mut literal=fixture("C"," Ω%_ ' ","",false,0,1);let (records,errors,_)=selected_run(vec![literal.clone()],vec![]," Ω%_ ' ",2);assert!(errors.is_empty());assert_eq!(records.iter().filter(|(id,_)|id.kind=="PersistedMenu").count(),1);
        let mut exact=request();exact.operation="acquire".into();exact.identity=Some(menu_identity(&literal,"C").unwrap());let baseline=serde_json::to_value(handle(exact.clone())).unwrap();exact.state["_pluginSettings"]=json!({"menuIdPrefix":false});assert_eq!(serde_json::to_value(handle(exact)).unwrap()["action"],baseline["action"]);
        literal["SysRevID"]=json!(1.5);let mut invalid=request();invalid.response=Some(json!({"returnObj":{"Results":[literal]}}));assert!(matches!(receive(invalid),GuestReply::Failure {..}));
    }
    #[test]
    fn ancestor_body_missing_changed_unauthorized_or_denied_keeps_partial_selected_roots() {
        let parent=fixture("Peer","Parent","",false,10,1);let child=fixture("C","Selected","Parent",false,0,2);
        for defect in ["missing","revision","rowid","unauthorized"] {
            let mut bodies=vec![parent.clone()];if defect=="missing" {bodies.clear();}else if defect=="revision" {bodies[0]["SysRevID"]=json!(9007199254741999u64);}else if defect=="rowid" {bodies[0]["SysRowID"]=json!("00000000-0000-0000-0000-999999999999");}else {bodies[0]["CompanyVisibility"]=json!(0);}
            let (records,errors,_)=selected_run(vec![parent.clone(),child.clone()],bodies,"Selected",1);assert!(records.iter().any(|(id,_)|id.keys.get("MenuID").map(String::as_str)==Some("Selected")));assert!(!errors.is_empty());assert_eq!(restore_index(&records)["dependencyBodiesComplete"],false);assert_eq!(restore_index(&records)["restoreOrderComplete"],false);
            assert!(errors.iter().any(|error|error.code==if defect=="missing" {"MENU_ANCESTOR_MISSING"}else if defect=="unauthorized" {"MENU_ANCESTOR_UNAUTHORIZED"}else{"MENU_ANCESTOR_CHANGED"}));
        }
        let mut r=request();r.page_size=Some(1);r.state["_pluginSettings"]=json!({"menuIdPrefix":"Selected"});r.state["phase"]=json!("ancestors");r.state["requiredDependencies"]=json!([parent]);r.state["dependencyBodies"]=json!([]);r.state["dependencyErrors"]=json!([]);r.state["finalPage"]=json!({"rows":[child],"graph":[],"rootBodiesComplete":true,"_selectionDiagnostics":[],"next":null});r.error=Some(Diagnostic::new("DENIED","Ancestor read denied"));
        let GuestReply::PreparedPage {state,..}=handle(r)else{panic!()};assert_eq!(state["rows"].as_array().unwrap().len(),1);assert_eq!(state["dependencyBodiesComplete"],false);assert!(state["_selectionDiagnostics"].as_array().unwrap().iter().any(|error|error["code"]=="MENU_ANCESTOR_READ_FAILED"));
    }
    #[test]
    fn selected_parent_precedence_ambiguity_missing_cycles_and_unrelated_branches_remain_explicit() {
        let rows=vec![fixture("","P","",false,10,1),fixture("C","P","",false,0,2),fixture("C","Selected","P",false,0,3),fixture("C","UnrelatedLoop","UnrelatedLoop",false,0,4)];let (records,errors,_)=selected_run(rows.clone(),rows.clone(),"Selected",1);assert!(errors.is_empty());assert_eq!(restore_index(&records)["restoreOrderComplete"],true);assert_eq!(records.iter().filter(|(id,_)|id.kind=="PersistedMenu" && id.keys["MenuID"]=="P").count(),1);assert!(records.iter().filter(|(id,_)|id.kind=="PersistedMenu" && id.keys["MenuID"]=="P").all(|(id,_)|id.keys["Company"]=="C"));
        for (rows,code) in [(vec![fixture("Peer","P","",false,10,1),fixture("Other","P","",false,10,2),fixture("C","Selected","P",false,0,3)],"MENU_PARENT_AMBIGUOUS"),(vec![fixture("C","Selected","Missing",false,0,1)],"MENU_PARENT_MISSING"),(vec![fixture("C","Selected","P",false,0,1),fixture("C","P","Selected",false,0,2)],"MENU_PARENT_CYCLE")] {
            let (records,errors,_)=selected_run(rows.clone(),rows,"Selected",1);assert!(errors.iter().any(|error|error.code==code));assert_eq!(restore_index(&records)["restoreOrderComplete"],false);
        }
    }
    #[test]
    fn menu_selection_skips_unselected_rows_and_freezes_scope_states() {
        // Unselected rows are not validated and produce no diagnostics.
        let mut bad=fixture("C","Other","",false,0,1);bad["MenuDesc"]=json!("PRIVATE_EXCLUDED_PAYLOAD\u{1}");let good=fixture("C","Selected","",false,0,2);
        let (records,errors,_)=selected_run(vec![bad,good.clone()],vec![],"Selected",1);assert!(records.iter().any(|(id,_)|id.keys.get("MenuID").map(String::as_str)==Some("Selected")));assert!(errors.is_empty(),"{errors:?}");assert!(!serde_json::to_string(&records).unwrap().contains("PRIVATE_EXCLUDED_PAYLOAD"));
        let mut initial=request();initial.state=json!({"_pluginSettings":{"menuIdPrefix":"Selected"}});initial.response=None;let GuestReply::Action {state,..}=handle(initial.clone())else{panic!()};assert_eq!(state["_pluginSettings"]["menuIdPrefix"],"Selected");initial.state=state;initial.response=Some(json!({"parameters":{"companyID":"C","tenantID":""}}));let GuestReply::Action {state,..}=handle(initial)else{panic!()};assert_eq!(state["_pluginSettings"]["menuIdPrefix"],"Selected");
        for value in [Value::Null,json!(false),json!(2),json!([])] {let mut invalid=request();invalid.state["_pluginSettings"]=json!({"menuIdPrefix":value});assert!(matches!(handle(invalid),GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));}
    }
    #[test]
    fn query_scopes_shared_owners_before_paging_and_escapes_literals() {
        let mut r = request();
        r.company = "C'1".into();
        let q = query(&r, false).unwrap();
        let criteria = q["queryDS"]["QueryWhereItem"].as_array().unwrap();
        assert_eq!(criteria[0]["RValue"], "N'C''1'");
        assert!(
            criteria
                .iter()
                .any(|c| c["FieldName"] == "CompanyVisibility" && c["RValue"] == "10")
        );
        assert!(
            criteria
                .iter()
                .any(|c| c["FieldName"] == "CompanyVisibility" && c["RValue"] == "5")
        );
        assert!(criteria.iter().any(|c| c["RValue"] == "N'Peer'"));
        assert!(!criteria.iter().any(|c| matches!(
            c["FieldName"].as_str(),
            Some("Enabled" | "SystemFlag" | "DoNotDisplayInMenu")
        )));
        r.cursor = Some("{}".into());
        assert!(query(&r, false).is_err());
    }
    #[test]
    fn receipt_checks_scope_and_fetches_next_page_before_xml_work() {
        let mut r = request();
        r.response = Some(
            json!({"returnObj":{"Results":[menu("Peer","A","ROOT",false,5),menu("Elsewhere","B","ROOT",false,10)]}}),
        );
        let GuestReply::PreparedPage {
            state,
            next_cursor,
            next_state,
        } = receive(r.clone())
        else {
            panic!()
        };
        assert!(next_cursor.is_some());
        assert_eq!(next_state["graph"].as_array().unwrap().len(), 2);
        assert!(state["rows"][0].get("herbertSources").is_none());
        r.response.as_mut().unwrap()["returnObj"]["Results"][0]["CompanyVisibility"] = json!(0);
        assert!(matches!(receive(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn stock_snapshot_does_not_create_one_artifact_per_system_menu() {
        let mut r = request();
        r.operation = "materialize".into();
        r.state = json!({"rows":[menu("","ROOT","",true,10),menu("","STOCK","ROOT",true,10),menu("C","A","ROOT",false,0)],"page":1,"next":null});
        let GuestReply::InventoryData { records, .. } = materialize(r) else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        let snapshot = &records
            .iter()
            .find(|(id, _)| id.kind == "SystemMenuSnapshot")
            .unwrap()
            .1;
        assert_eq!(snapshot["Menu"].as_array().unwrap().len(), 2);
        let custom = &records
            .iter()
            .find(|(id, _)| id.kind == "PersistedMenu")
            .unwrap()
            .1;
        let xml = custom["herbertSources"]["files"][0]["text"]
            .as_str()
            .unwrap();
        assert!(xml.contains("Ω &amp; &lt; &gt;"));
        assert!(xml.contains("<Enabled>false</Enabled>"));
        assert_eq!(custom["source"]["Alias"], "one~two");
        assert_eq!(custom["Menu"][0]["Alias"], "one\ntwo");
    }
    #[test]
    fn parent_order_preserves_variants_and_reports_missing_ambiguous_cycles() {
        let rows = vec![
            menu("C", "CHILD", "P", false, 0),
            menu("C", "P", "ROOT", false, 0),
            menu("", "ROOT", "", true, 10),
            menu("", "P", "ROOT", true, 10),
        ];
        let (g, d) = restore_graph(&rows, "C");
        assert!(d.is_empty());
        assert_eq!(g["customRestoreOrder"][0]["keys"]["MenuID"], "P");
        assert_eq!(g["menus"].as_array().unwrap().len(), 4);
        let rows = vec![
            menu("C", "A", "P", false, 0),
            menu("Peer", "P", "", true, 10),
            menu("Other", "P", "", true, 10),
            menu("C", "MISSING", "NONE", false, 0),
            menu("C", "LOOP", "LOOP", false, 0),
        ];
        let (_, d) = restore_graph(&rows, "C");
        for code in [
            "MENU_PARENT_AMBIGUOUS",
            "MENU_PARENT_MISSING",
            "MENU_PARENT_CYCLE",
        ] {
            assert!(d.iter().any(|d| d.code == code));
        }
    }

    #[test]
    fn wave1_tenant_pages_validate_before_extending_scope() {
        let mut r=request();r.state["phase"]=json!("companies");r.state["companies"]=json!(["A"]);r.state["companyAfter"]=json!("A");
        for rows in [json!([{"Company":"A"},{"Company":"B"}]),json!([{"Company":"B"},{"Company":"B"}]),
            json!([{"Company":""}]),json!([{"Company":1}]),json!([{}]),
            json!([{"Company":"B"},{"Company":"C"},{"Company":"D"}])] {
            r.response=Some(json!({"returnObj":{"Results":rows}}));
            assert!(matches!(handle(r.clone()),GuestReply::Failure{..}));
        }
        for size in [0,2147483648] {
            let mut invalid=r.clone();invalid.page_size=Some(size);invalid.response=Some(json!({"returnObj":{"Results":[]}}));
            assert!(matches!(handle(invalid),GuestReply::Failure{..}));
        }
        r.response=Some(json!({"returnObj":{"Results":[{"Company":"B"},{"Company":"C"}]}}));
        let GuestReply::Action{state,..}=handle(r.clone()) else {panic!()};
        assert_eq!(state["companies"],json!(["A","B","C"]));assert_eq!(state["companyAfter"],"C");
        r.state=state;
        assert!(matches!(handle(r.clone()),GuestReply::Failure{..}));
        r.response=Some(json!({"returnObj":{"Results":[{"Company":"D"}]}}));
        let GuestReply::Action{state,action:HostAction::NativeRead{arguments,..}}=handle(r) else {panic!()};
        assert_eq!(state["companies"],json!(["A","B","C","D"]));assert_eq!(state["companiesDone"],true);
        assert_eq!(state["phase"],"page");
        assert!(arguments["queryDS"]["QueryWhereItem"].as_array().unwrap().iter().any(|c|c["RValue"]=="N'D'"));
    }

}
