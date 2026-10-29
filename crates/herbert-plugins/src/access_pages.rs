//! Access and assignments caller-visible native definitions and explicit assignment projections.
//! Complete native graphs are acquired once per family, then grouped locally.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const SURFACES: &[(&str, &str)] = &[
    ("SecurityDefinition", "Security"),
    ("ObjectSecurity", "ObjectSecurity"),
    ("SecurityGroup", "SecGroup"),
    ("ColumnSecurity", "SecColumn"),
    ("AccessScope", "AccessScope"),
    ("UserAccessAssignment", "UserFile"),
    ("UserCompanyAssignment", "UserFile"),
    ("APIKeyScopeAssignment", "ApiKey"),
    ("UserExternalCompanyAssignment", "UserFile"),
];
const USER_FIELDS: &str =
    "UserID,GroupList,CompList,SecurityMgr,GlbCompSM,AccessScopeID,UserDisabled,SysRowID,SysRevID";
const COMPANY_FIELDS: &str = "UserID,Company,CurPlant,PlantList,SysRowID,SysRevID";
const KEY_FIELDS: &str =
    "Company,KeyID,Name,AccessScopeID,Enabled,ExpiresOn,AllCompanies,SystemFlag,SysRowID,SysRevID";
const EXTERNAL_COMPANY_FIELDS: &str = "UserID,Company,ExtCompID,CurPlant,SysRowID,SysRevID";
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("ACCESS_GRAPH_INVALID", s)
}

fn selection(state:&Value)->Result<(&str,&str),Diagnostic>{let settings=&state["_pluginSettings"];let family=match settings.get("accessFamily"){None=>"all",Some(Value::String(v))if matches!(v.as_str(),"all"|"definitions"|"userAssignments"|"apiKeyAssignments")=>v,_=>return Err(err("Select a declared access family."))};let prefix=match settings.get("userIdPrefix"){None=>"",Some(Value::String(v))=>v,_=>return Err(err("The user ID prefix must be a string."))};Ok((family,prefix))}
/// The prefix's leading run before any LIKE wildcard, bracket or non-ASCII character, for a
/// server-side starts-with superset that needs no escape syntax. Non-ASCII stops the stem, as in
/// the SQL plugins, so a lossy conversion can never narrow the read. None when nothing narrows it.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
/// The literal user prefix for discovery of the three user-assignment projections; blank otherwise.
fn user_prefix(r:&GuestRequest,index:usize)->Result<String,Diagnostic>{Ok(if r.identity.is_none()&&matches!(index,5|6|8){selection(&r.state)?.1.to_owned()}else{String::new()})}
fn selected_surface(i:usize,state:&Value)->Result<bool,Diagnostic>{let(family,_)=selection(state)?;Ok(family=="all"||family==if i<5{"definitions"}else if i==7{"apiKeyAssignments"}else{"userAssignments"})}
fn selected_identity(identity:&Identity,state:&Value)->Result<bool,Diagnostic>{let i=SURFACES.iter().position(|(kind,_)|*kind==identity.kind).or_else(||matches!(identity.kind.as_str(),"NativeSecuritySourceSupplement"|"NativeAccessSupplementary").then_some(0)).ok_or_else(||err("Unknown access output owner."))?;let (_,prefix)=selection(state)?;Ok(selected_surface(i,state)?&&(!matches!(i,5|6|8)||identity.keys.get("UserID").is_some_and(|user|user.starts_with(prefix))))}

fn selection_settings(mut state:Value,from:&Value,errors:Vec<Diagnostic>)->Value {
    if !state.is_object(){state=json!({});}
    if let Some(settings)=from.get("_pluginSettings"){state["_pluginSettings"]=settings.clone();}
    let mut all:Vec<Value>=state.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
    for value in from.get("diagnostics").and_then(Value::as_array).into_iter().flatten().cloned().chain(errors.into_iter().map(|d|json!(d))){if !all.contains(&value){all.push(value);}}
    if !all.is_empty(){state["diagnostics"]=json!(all);}
    state
}
fn select_records(records:Vec<(Identity,Value)>,from:&Value)->Result<(Vec<(Identity,Value)>,Vec<Diagnostic>),Diagnostic>{
    let mut selected=vec![];let mut diagnostics=vec![];
    for (identity,data) in records {if selected_identity(&identity,from)? {selected.push((identity,data));} else {for value in data.pointer("/herbertCollectionStatus/diagnostics").and_then(Value::as_array).into_iter().flatten(){let mut d:Diagnostic=serde_json::from_value(value.clone()).map_err(|_|err("Invalid acquired selection diagnostic."))?;d.identity=Some(identity.clone());diagnostics.push(d);}}}
    Ok((selected,diagnostics))
}
fn selected_reply(reply:GuestReply,from:&Value)->Result<GuestReply,Diagnostic>{
    let filter_state=|mut state:Value|->Result<Value,Diagnostic>{if let Some(value)=state.get("records"){let records=serde_json::from_value(value.clone()).map_err(|_|err("Invalid selected output owners."))?;let (records,errors)=select_records(records,from)?;state["records"]=json!(records);state=selection_settings(state,from,errors);}else{state=selection_settings(state,from,vec![]);}Ok(state)};
    Ok(match reply {
        GuestReply::Action{action,state}=>GuestReply::Action{action,state:selection_settings(state,from,vec![])},
        GuestReply::PreparedPage{state,next_cursor,next_state}=>GuestReply::PreparedPage{state:filter_state(state)?,next_cursor,next_state:selection_settings(next_state,from,vec![])},
        GuestReply::ReadyRecords{records,state}=>{let(records,errors)=select_records(records,from)?;GuestReply::ReadyRecords{records,state:selection_settings(state,from,errors)}},
        GuestReply::InventoryData{records,inventory}=>{let(records,errors)=select_records(records,from)?;let state=selection_settings(json!({}),from,errors);GuestReply::InventoryData{records,inventory:Box::new(selected_reply(*inventory,&state)?)}},
        GuestReply::Inventory{identities,next_cursor,authoritative,mut diagnostics}=>{let mut selected=vec![];for identity in identities{if selected_identity(&identity,from)?{selected.push(identity);}}for value in from.get("diagnostics").and_then(Value::as_array).into_iter().flatten(){let d:Diagnostic=serde_json::from_value(value.clone()).map_err(|_|err("Invalid acquired selection diagnostic."))?;if !diagnostics.iter().any(|old|json!(old)==json!(d)){diagnostics.push(d);}}GuestReply::Inventory{identities:selected,next_cursor,authoritative:authoritative&&diagnostics.is_empty(),diagnostics}},
        other=>other,
    })
}

pub fn methods() -> Vec<(String, String)> {
    let mut m = vec![("Ice.Lib.SessionModSvc".into(), "GetBaseLevel".into())];
    m.extend(
        SURFACES[..5]
            .iter()
            .map(|(_, s)| (format!("Ice.BO.{s}Svc"), "GetRows".into())),
    );
    m
}
pub fn entities() -> Vec<(String, String)> {
    [
        ("UserFile", "UserFiles"),
        ("UserFile", "UserComps"),
        ("UserFile", "UserCompExts"),
        ("ApiKey", "APIKeys"),
    ]
    .into_iter()
    .map(|(s, e)| (format!("Ice.BO.{s}Svc"), e.into()))
    .collect()
}
pub fn policies() -> Vec<NativeResponsePolicy> {
    let mut policies = crate::response_policy::for_methods(&methods());
    for p in &mut policies {
        if let Some(t) = &mut p.tables {
            let names: &[&str] = match p.service.as_str() {
                "Ice.BO.SecuritySvc" => &["Security"],
                "Ice.BO.ObjectSecuritySvc" => &["Security", "ObjectSecurityMethod"],
                "Ice.BO.SecGroupSvc" => &["SecGroup"],
                "Ice.BO.SecColumnSvc" => &["SecColumn"],
                "Ice.BO.AccessScopeSvc" => {
                    &["AccessScope", "AccessScopeEntity", "AccessScopeBOMethod"]
                }
                _ => &[],
            };
            // Native visibility admits shared rows whose stored Company differs
            // from the selected company. Validate them in the guest and retain
            // that physical owner in the existing SharedCompany identity scope.
            t.unscoped_tables.extend(names.iter().map(|s| (*s).into()));
        }
    }
    policies
}
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_str(), b.as_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => a == b,
    }
}
fn scope(company: &str) -> Scope {
    if company.is_empty() {
        Scope::Global
    } else {
        Scope::Company {
            company: company.into(),
        }
    }
}
fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str, Diagnostic> {
    row[key]
        .as_str()
        .ok_or_else(|| err("An Epicor access identity field is absent or has an invalid type."))
}
fn identity(kind: &str, row: &Value, keys: &[&str], company: &str) -> Result<Identity, Diagnostic> {
    Ok(Identity {
        category: "access-and-assignments".into(),
        kind: kind.into(),
        scope: scope(company),
        keys: keys
            .iter()
            .map(|k| Ok(((*k).into(), string(row, k)?.into())))
            .collect::<Result<_, Diagnostic>>()?,
    })
}
fn native_owner(row: &Value, company: &str, inherited_scope: bool) -> Result<String, Diagnostic> {
    let c = string(row, "Company")?;
    if !c.is_empty()
        && !c.eq_ignore_ascii_case(company)
        && row["AllCompanies"] != true
        && !matches!(row["CompanyVisibility"].as_u64(), Some(5 | 10))
        && !inherited_scope
    {
        return Err(err("An Epicor access row belongs to another company."));
    }
    Ok(c.into())
}
fn owner(row: &Value, company: &str) -> Result<String, Diagnostic> {
    native_owner(row, company, false)
}
fn applicable_scope(owner: &str, selected: &str) -> Scope {
    if owner.is_empty() {
        Scope::Global
    } else if owner.eq_ignore_ascii_case(selected) {
        Scope::Company {
            company: owner.into(),
        }
    } else {
        Scope::SharedCompany {
            company: selected.into(),
            owner_company: owner.into(),
        }
    }
}
fn rows<'a>(body: &'a Value, table: &str) -> Result<&'a Vec<Value>, Diagnostic> {
    body[table]
        .as_array()
        .ok_or_else(|| err("The Epicor access record set omitted a required table."))
}
fn checked<'a>(
    body: &'a Value,
    table: &str,
    company: &str,
    marked: bool,
) -> Result<&'a Vec<Value>, Diagnostic> {
    let a = rows(body, table)?;
    let mut ids = BTreeSet::new();
    for r in a {
        native_owner(
            r,
            company,
            matches!(table, "AccessScopeEntity" | "AccessScopeBOMethod"),
        )?;
        if marked && r["SystemFlag"] != false {
            return Err(err(
                "The access record set contains an unverified or system-owned definition.",
            ));
        }
        let id = string(r, "SysRowID")?;
        if id.is_empty() || !ids.insert(id.to_ascii_lowercase()) {
            return Err(err(
                "An Epicor access physical identity is absent or repeated.",
            ));
        }
    }
    Ok(a)
}
fn complete(
    service: &str,
    kind: &str,
    ds: Value,
    context: Value,
    version: &str,
    mut diagnostics: Vec<Diagnostic>,
) -> Value {
    let mut data = json!({"service":service,"definition":ds,"callerVisibility":"native caller-visible definitions only; inaccessible records are not inferred absent","herbertParentContext":context});
    data["herbertRestore"] = if kind == "NativeSecuritySourceSupplement" {
        let row = &data["definition"]["Security"][0];
        let parent = row["ParentSecCode"].as_str().unwrap_or("");
        json!({"contract":"source-only-supplement","nativeSourceService":"Ice.BO.SecuritySvc","relatedDefinition":{"category":"access-and-assignments","kind":"ObjectSecurity","nativeKey":{"Company":row["Company"],"SecCode":if parent.eq_ignore_ascii_case("OBJECT"){row["SecCode"].clone()}else{row["ParentSecCode"].clone()}},"resolution":"unverified-reference"},"instruction":"This is supplemental native source, not a standalone SecuritySvc import. SecuritySvc GetNew/Update accepts only menu or collaboration security codes. If the matching ObjectSecurity artifact was collected, use its ObjectSecuritySvc recovery contract for the native object root or permission. This reference does not establish that the artifact or destination parent exists. Additional SecuritySvc fields remain source evidence; no separate native write contract for them is claimed."})
    } else if kind == "NativeAccessSupplementary" {
        json!({"contract":"source-only-unverified-relations","instruction":"The native source is retained. Its table relationships and native recovery contract are unverified. No GetNew/Update or import replay is claimed for this supplemental data."})
    } else if matches!(
        kind,
        "UserAccessAssignment"
            | "UserCompanyAssignment"
            | "UserExternalCompanyAssignment"
            | "APIKeyScopeAssignment"
    ) {
        json!({"contract":"assignment-projection","instruction":"Resolve the existing principal or API key by its native identity, read the current native tableset, apply only the preserved assignment fields through native Update and verify readback. Credentials, personal data and other fields were not collected; this is not a whole-user or key backup."})
    } else {
        json!({"contract":"native-tableset","instruction":"Use the native service GetNew/Update methods for supported missing authored rows or Update for existing rows. Preserve exact native Company and parent identities. A parent context is a dependency, not a row to insert or overwrite. Any herbertUnassociatedNativeData is supplemental source with unverified relationships and no proven native write contract; do not replay it as a native table. Native audit/revision/default fields can change during recovery; read back permissions and relationships."})
    };
    let mut binaries = vec![];
    if kind == "AccessScope"
        && diagnostics.is_empty()
        && data["definition"]["AccessScope"]
            .as_array()
            .is_some_and(|v| v.len() == 1)
    {
        match crate::access_archive::build(&data["definition"], version) {
            Ok(bytes) => {
                use sha2::{Digest, Sha256};
                binaries.push(json!({"name":"access-scope.eas","bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}));
                data["herbertRestore"] = json!({"service":"Ice.BO.AccessScopeSvc","method":"ImportAccessScope","file":"access-scope.eas","version":version,"instruction":"Supply these bytes as importBytes and select the target AccessScopeID. Native import creates target identities/audit values and uses its current-session scope defaults. Restore assignments separately only after reviewing target principals and scope semantics."});
            }
            Err(d) => diagnostics.push(d),
        }
    }
    let text = serde_json::to_string_pretty(&data).unwrap();
    data["herbertSources"] =
        json!({"files":[{"name":"access-definition.json","text":text}],"binaryFiles":binaries});
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] = json!({"completeness":if diagnostics.is_empty(){"collected"}else{"partial"},"diagnostics":diagnostics});
    data
}
fn native_records(
    index: usize,
    body: &Value,
    company: &str,
    version: &str,
) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let (kind, short) = SURFACES[index];
    let service = format!("Ice.BO.{short}Svc");
    let (root, keys, children): (&str, Vec<&str>, Vec<&str>) = match index {
        0 => ("Security", vec!["Company", "SecCode"], vec![]),
        1 => (
            "Security",
            vec!["Company", "SecCode"],
            vec!["ObjectSecurityMethod"],
        ),
        2 => ("SecGroup", vec!["Company", "SecGroupCode"], vec![]),
        3 => (
            "SecColumn",
            vec![
                "Company",
                "DatasourceType",
                "SchemaName",
                "TableName",
                "ColumnName",
            ],
            vec![],
        ),
        4 => (
            "AccessScope",
            vec!["Company", "AccessScopeID"],
            vec!["AccessScopeEntity", "AccessScopeBOMethod"],
        ),
        _ => return Err(err("Invalid Epicor access surface.")),
    };
    let roots = checked(body, root, company, index != 3)?;
    for child in &children {
        checked(body, child, company, true)?;
    }
    let mut grouped: BTreeMap<(String, String), Value> = BTreeMap::new();
    let code = if index == 4 {
        "AccessScopeID"
    } else {
        "SecCode"
    };
    let mut logical = BTreeSet::new();
    for row in roots {
        let c = owner(row, company)?;
        let row_kind = if index == 0 {
            let parent = string(row, "ParentSecCode")?;
            if parent.eq_ignore_ascii_case("MENU") || parent.eq_ignore_ascii_case("COLLABORATE") {
                kind
            } else {
                "NativeSecuritySourceSupplement"
            }
        } else {
            kind
        };
        let mut id = identity(row_kind, row, &keys, &c)?;
        id.scope = applicable_scope(&c, company);
        if !logical.insert(serde_json::to_string(&id).unwrap().to_ascii_lowercase()) {
            return Err(err("An Epicor access logical identity is repeated."));
        }
        let mut ds = json!({});
        ds[root] = json!([row]);
        for c in &children {
            ds[*c] = json!([]);
        }
        ds["ExtensionTables"] = body.get("ExtensionTables").cloned().unwrap_or(json!([]));
        // Single-row families have keys beyond Company/code (column security).
        let group_key = (
            string(row, "Company")?.to_ascii_lowercase(),
            if children.is_empty() {
                serde_json::to_string(&id).unwrap()
            } else {
                string(row, code)?.to_ascii_lowercase()
            },
        );
        grouped.insert(group_key, json!({"id":id,"ds":ds,"context":null}));
    }
    for table in &children {
        for row in rows(body, table)? {
            let parent_field = if index == 1 {
                "ParentSecCode"
            } else {
                "AccessScopeID"
            };
            let key = (
                string(row, "Company")?.to_ascii_lowercase(),
                string(row, parent_field)?.to_ascii_lowercase(),
            );
            if !grouped.contains_key(&key) {
                // Native GetRows can return authored children with no authored
                // root. Keep native child scope and a minimal dependency reference.
                let parent = json!({"Company":row["Company"],code:row[parent_field]});
                let c = native_owner(row, company, index == 4)?;
                let mut id = identity(kind, &parent, &keys, &c)?;
                id.scope = applicable_scope(&c, company);
                let mut ds = json!({root:[],"ExtensionTables":[]});
                for c in &children {
                    ds[*c] = json!([]);
                }
                grouped.insert(key.clone(),json!({"id":id,"ds":ds,"context":{"nativeParentKey":parent,"authoredParentCollected":false,"instruction":"Resolve the native parent before recovery. No stock parent body or ownership is inferred from this child reference."}}));
            }
            grouped.get_mut(&key).unwrap()["ds"][*table]
                .as_array_mut()
                .unwrap()
                .push(row.clone());
        }
    }
    let known: BTreeSet<_> = std::iter::once(root)
        .chain(children.iter().copied())
        .chain(["ExtensionTables"])
        .collect();
    let supplemental: serde_json::Map<String, Value> = body
        .as_object()
        .ok_or_else(|| err("Invalid Epicor access body."))?
        .iter()
        .filter(|(k, v)| {
            (!known.contains(k.as_str()) || k.as_str() == "ExtensionTables")
                && v.as_array().is_none_or(|a| !a.is_empty())
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = vec![];
    if grouped.is_empty() && !supplemental.is_empty() {
        let id = Identity {
            category: "access-and-assignments".into(),
            kind: "NativeAccessSupplementary".into(),
            scope: scope(company),
            keys: BTreeMap::from([("Service".into(), service.clone())]),
        };
        out.push((id,complete(&service,"NativeAccessSupplementary",json!({"herbertUnassociatedNativeData":supplemental}),Value::Null,version,vec![Diagnostic::new("ACCESS_NATIVE_RELATION_UNKNOWN","Epicor data outside known definition tables is retained as source. Its relationship and recovery contract are unverified.")])));
    }
    for (_, mut group) in grouped {
        let id: Identity = serde_json::from_value(group["id"].take())
            .map_err(|_| err("Invalid access output identity."))?;
        let mut diagnostics = vec![];
        if !supplemental.is_empty() {
            group["ds"]["herbertUnassociatedNativeData"] = Value::Object(supplemental.clone());
            diagnostics.push(Diagnostic::new("ACCESS_NATIVE_RELATION_UNKNOWN","Unexpected Epicor tables or metadata were retained as unassociated source; their relationship closure is unverified."));
        }
        if index == 4 {
            let entities = rows(&group["ds"], "AccessScopeEntity")?;
            for method in rows(&group["ds"], "AccessScopeBOMethod")? {
                if !entities.iter().any(|e| {
                    ["Company", "AccessScopeID", "EntityType", "EntityID"]
                        .iter()
                        .all(|k| same(&e[*k], &method[*k]))
                }) {
                    diagnostics.push(Diagnostic::new("ACCESS_ENTITY_PARENT_NOT_COLLECTED","A custom method references an entity outside the collected custom entity set. Its exact parent keys remain in the method; restore requires that Epicor entity."));
                }
            }
        }
        let record_kind = id.kind.clone();
        out.push((
            id,
            complete(
                &service,
                &record_kind,
                group["ds"].take(),
                group["context"].take(),
                version,
                diagnostics,
            ),
        ));
    }
    Ok(out)
}
fn assignment(index: usize) -> (&'static str, &'static str, &'static str) {
    match index {
        5 => ("UserFiles", USER_FIELDS, "UserFile"),
        6 => ("UserComps", COMPANY_FIELDS, "UserComp"),
        7 => ("APIKeys", KEY_FIELDS, "APIKey"),
        8 => ("UserCompExts", EXTERNAL_COMPANY_FIELDS, "UserCompExt"),
        _ => unreachable!(),
    }
}
fn guid(s: &str) -> bool {
    let parts: Vec<_> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn assignment_records(
    index: usize,
    source: &[Value],
    company: &str,
) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let (kind, s) = SURFACES[index];
    let (_, fields, table) = assignment(index);
    let allowed: BTreeSet<_> = fields.split(',').collect();
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    for r in source {
        if r.as_object()
            .is_none_or(|m| m.keys().any(|k| !allowed.contains(k.as_str())))
        {
            return Err(err(
                "An assignment projection returned an undeclared field.",
            ));
        }
        let c = if index == 5 {
            "".into()
        } else {
            owner(r, company)?
        };
        if matches!(index, 6 | 8) && !c.eq_ignore_ascii_case(company) {
            return Err(err("A user-company assignment belongs to another company."));
        }
        let keys: &[&str] = match index {
            5 => &["UserID"],
            6 => &["UserID", "Company"],
            8 => &["UserID", "Company", "ExtCompID"],
            _ => &["Company", "KeyID"],
        };
        let mut id = identity(kind, r, keys, &c)?;
        id.scope = applicable_scope(&c, company);
        if !seen.insert(serde_json::to_string(&id).unwrap().to_ascii_lowercase()) {
            return Err(err("An Epicor assignment identity is repeated."));
        }
        for field in &allowed {
            if r.get(*field).is_none() {
                return Err(err("An assignment projection omitted a declared field."));
            }
        }
        let data = complete(
            &format!("Ice.BO.{s}Svc"),
            kind,
            json!({table:[r]}),
            json!({"projectionFields":fields,"scope":"Native caller-visible global user assignments and selected-company relationships; referenced principals and groups can require separate recovery."}),
            "",
            vec![],
        );
        out.push((id, data));
    }
    Ok(out)
}
fn next(index: usize) -> Option<String> {
    (index + 1 < SURFACES.len()).then(|| json!({"surface":index+1}).to_string())
}
fn finish(
    r: GuestRequest,
    records: Vec<(Identity, Value)>,
    index: usize,
) -> Result<GuestReply, Diagnostic> {
    if let Some(target) = r.identity {
        let data = records
            .into_iter()
            .find(|(id, _)| *id == target)
            .ok_or_else(|| {
                Diagnostic::new(
                    "ACCESS_NOT_FOUND",
                    "No Epicor user-visible access definition matches this exact identity.",
                )
            })?
            .1;
        let diagnostics =
            serde_json::from_value(data["herbertCollectionStatus"]["diagnostics"].clone())
                .unwrap_or_default();
        return Ok(GuestReply::Definition {
            identity: target,
            format: "native-access-definition".into(),
            completeness: if data["herbertCollectionStatus"]["completeness"] == "collected" {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
            definition: data,
        });
    }
    let state = json!({"phase":"output","records":records,"index":0,"next":next(index),"bulk":r.state["bulk"]});
    if r.state["prepare"] == true {
        Ok(GuestReply::PreparedPage {
            state,
            next_cursor: next(index),
            next_state: json!({}),
        })
    } else {
        output(state)
    }
}
fn output(mut s: Value) -> Result<GuestReply, Diagnostic> {
    // Every record is already complete. Hand the page to the host once instead
    // of serializing its remaining definitions across every guest continuation.
    // Old prepared checkpoints retain the single-record continuation below.
    if s["bulk"] == true {
        let records: Vec<(Identity, Value)> = serde_json::from_value(s["records"].take())
            .map_err(|_| err("Invalid access output records."))?;
        return Ok(GuestReply::InventoryData {
            inventory: Box::new(GuestReply::Inventory {
                identities: records.iter().map(|(id, _)| id.clone()).collect(),
                next_cursor: s["next"].as_str().map(str::to_owned),
                authoritative: true,
                diagnostics: vec![],
            }),
            records,
        });
    }
    let i = s["index"]
        .as_u64()
        .ok_or_else(|| err("Invalid access output saved progress."))? as usize;
    let count = s["records"]
        .as_array()
        .ok_or_else(|| err("Invalid access output records."))?
        .len();
    if i == count {
        return Ok(GuestReply::Inventory {
            identities: vec![],
            authoritative: true,
            diagnostics: vec![],
            next_cursor: s["next"].as_str().map(str::to_owned),
        });
    }
    if i > count {
        return Err(err("Invalid access output position."));
    }
    let record = serde_json::from_value(s["records"][i].take())
        .map_err(|_| err("Invalid access output record."))?;
    s["index"] = json!(i + 1);
    Ok(GuestReply::ReadyRecords {
        records: vec![record],
        state: s,
    })
}
fn surface_action(mut r: GuestRequest, index: usize) -> Result<GuestReply, Diagnostic> {
    let (_, service) = SURFACES[index];
    r.state["phase"] = json!("rows");
    let company = r.company.replace('\'', "''");
    if index < 5 {
        let mut args = json!({"pageSize":0,"absolutePage":0});
        let tables: &[&str] = match index {
            0 => &["Security"],
            1 => &["Security", "ObjectSecurityMethod"],
            2 => &["SecGroup"],
            3 => &["SecColumn"],
            _ => &["AccessScope", "AccessScopeEntity", "AccessScopeBOMethod"],
        };
        for t in tables {
            args[format!("whereClause{t}")] = json!(if index == 4 && *t != "AccessScope" {
                "SystemFlag = false".to_string()
            } else {
                format!(
                    "(Company = '{company}' OR Company = '' OR CompanyVisibility = 5 OR CompanyVisibility = 10){}",
                    if index == 3 {
                        ""
                    } else {
                        " AND SystemFlag = false"
                    }
                )
            });
        }
        // Native GetRows uses 0 for the complete graph (as its own exporters do).
        // This is not a resource cap and does not apply root/child row windows.
        return Ok(crate::native(
            &format!("Ice.BO.{service}Svc"),
            "GetRows",
            args,
            false,
            r.state,
        ));
    }
    let (entity, fields, _) = assignment(index);
    let mut query = BTreeMap::from([
        ("$select".into(), fields.into()),
        ("$orderby".into(), "SysRowID".into()),
        ("$count".into(), "true".into()),
    ]);
    let mut filters = vec![];
    if matches!(index, 6 | 8) {
        filters.push(format!("Company eq '{company}'"));
    }
    // Server-side superset for the user prefix; the literal case-sensitive prefix is
    // applied locally. Exact acquisition, definitions and API keys are not narrowed.
    if let Some(stem) = like_stem(&user_prefix(&r, index)?) {
        filters.push(format!("startswith(UserID,'{}')", stem.replace('\'', "''")));
    }
    if let Some(after) = r.state["after"].as_str() {
        if !guid(after) {
            return Err(err(
                "The assignment keyset saved progress is not an Epicor GUID.",
            ));
        }
        filters.push(format!("SysRowID gt {after}"));
    }
    if !filters.is_empty() {
        query.insert("$filter".into(), filters.join(" and "));
    }
    // OData $top is Int32. Use the protocol's positive range and verify the
    // server count/continuation, rather than inventing an assignment count cap.
    // Keyset continuation uses the last received native SysRowID, with page=1
    // each time. A server may return fewer than $top; no offset is guessed from
    // the requested size. SQL/OData apply the same GUID ordering and predicate.
    Ok(GuestReply::Action {
        action: HostAction::NativeEntityPage {
            service: format!("Ice.BO.{service}Svc"),
            entity: entity.into(),
            query,
            page_size: i32::MAX as u32,
            page: 1,
        },
        state: r.state,
    })
}
fn drive(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if r.operation == "materialize" && r.state["phase"] == "output" {
        return output(r.state);
    }
    if !matches!(r.operation.as_str(), "discover" | "materialize" | "acquire") {
        return Err(Diagnostic::new(
            "UNSUPPORTED_OPERATION",
            "The Access and assignments plugin received a request it does not support. Make sure the plugin and Herbert come from the same release.",
        ));
    }
    if r.company.is_empty() {
        return Err(err("Select a company for Epicor access collection."));
    }
    if r.operation == "acquire" && r.identity.is_none() {
        return Err(Diagnostic::new(
            "IDENTITY_REQUIRED",
            "Select an exact Epicor access identity.",
        ));
    }
    if r.state["phase"].is_null() {
        let index = if let Some(id) = &r.identity {
            if id.category != "access-and-assignments" {
                return Err(err("The selected identity belongs to another category."));
            }
            let i = SURFACES
                .iter()
                .position(|(k, _)| *k == id.kind)
                .or_else(|| {
                    if id.kind == "NativeSecuritySourceSupplement" {
                        Some(0)
                    } else if id.kind == "NativeAccessSupplementary" {
                        SURFACES[..5].iter().position(|(_, s)| {
                            id.keys
                                .get("Service")
                                .is_some_and(|v| v == &format!("Ice.BO.{s}Svc"))
                        })
                    } else {
                        None
                    }
                })
                .ok_or_else(|| err("Unknown access definition kind."))?;
            let c = match &id.scope {
                Scope::Global => "",
                Scope::Company { company } if company.eq_ignore_ascii_case(&r.company) => company,
                Scope::SharedCompany {
                    company,
                    owner_company,
                } if company.eq_ignore_ascii_case(&r.company)
                    && !owner_company.is_empty()
                    && !owner_company.eq_ignore_ascii_case(company)
                    && (i < 5 || i == 7) =>
                {
                    owner_company
                }
                _ => return Err(err("The selected access identity is outside this company.")),
            };
            if id.keys.get("Company").is_some_and(|v| v != c)
                || (i == 5 && !matches!(id.scope, Scope::Global))
            {
                return Err(err("The Epicor Company key and scope disagree."));
            }
            i
        } else if let Some(cursor) = &r.cursor {
            let v: Value =
                serde_json::from_str(cursor).map_err(|_| err("Herbert could not continue this download from its saved progress (access paging). Start the download again."))?;
            let i = v["surface"]
                .as_u64()
                .filter(|n| *n < SURFACES.len() as u64)
                .ok_or_else(|| err("Herbert could not continue this download from its saved progress (access area). Start the download again."))? as usize;
            if v.as_object().is_none_or(|m| m.len() != 1) {
                return Err(err("Herbert could not continue this download from its saved progress (access paging details). Start the download again."));
            }
            i
        } else {
            0
        };
        r.state = json!({"phase":"version","surface":index,"prepare":r.state["_hostPreparePage"],"bulk":r.state["_hostIndependentWorkItems"],"page":1,"rows":[],"seen":[],"version":""});
        return Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetBaseLevel".into(),
                arguments: json!({}),
                export: false,
            },
            state: r.state,
        });
    }
    let index = r.state["surface"]
        .as_u64()
        .filter(|n| *n < SURFACES.len() as u64)
        .ok_or_else(|| err("Herbert could not continue this download from its saved progress (access area position). Start the download again."))? as usize;
    if r.state["phase"] == "version" {
        if let Some(diagnostic) = r.error.take() {
            r.state["versionDiagnostic"] = json!(diagnostic);
        }
        if r.error.is_none() {
            if let Some(v) = r.response.take() {
                let parts: [Option<u64>; 4] =
                    ["_Major", "_Minor", "_Build", "_Revision"].map(|k| v["returnObj"][k].as_u64());
                if parts.iter().all(Option::is_some) {
                    r.state["version"] = json!(parts.map(|n| n.unwrap().to_string()).join("."));
                }
            }
        }
        // Version failure affects only construction of native scope packages.
        r.error = None;
        return surface_action(r, index);
    }
    if r.state["phase"] != "rows" {
        return Err(err("Unknown access collection phase."));
    }
    if let Some(d) = r.error.take() {
        return Err(d);
    }
    let response = r
        .response
        .take()
        .ok_or_else(|| err("The access response is absent."))?;
    if index < 5 {
        if response["parameters"]["morePages"] != false {
            return Err(err(
                "The Epicor complete access record set did not confirm completeness.",
            ));
        }
        let mut records = native_records(
            index,
            &response["returnObj"],
            &r.company,
            r.state["version"].as_str().unwrap_or(""),
        )?;
        if index == 4 && !r.state["versionDiagnostic"].is_null() {
            for (_, data) in &mut records {
                if data["definition"]["AccessScope"]
                    .as_array()
                    .is_some_and(|roots| roots.len() == 1)
                {
                    data["herbertCollectionStatus"]["diagnostics"]
                        .as_array_mut()
                        .unwrap()
                        .push(r.state["versionDiagnostic"].clone());
                }
            }
        }
        return finish(r, records, index);
    }
    let incoming = response["value"]
        .as_array()
        .ok_or_else(|| err("The assignment response omitted rows."))?;
    let total = response["@odata.count"]
        .as_u64()
        .ok_or_else(|| err("The assignment response omitted its count."))?;
    let before = r.state["rows"]
        .as_array()
        .ok_or_else(|| err("Herbert could not continue this download from its saved progress (access assignment position). Start the download again."))?
        .len() as u64;
    if let Some(original) = r.state["total"].as_u64() {
        if before.checked_add(total) != Some(original) {
            return Err(err(
                "The remaining assignment count changed during collection.",
            ));
        }
    } else {
        r.state["total"] = json!(total);
    }
    let mut seen: BTreeSet<String> = serde_json::from_value(r.state["seen"].clone())
        .map_err(|_| err("Invalid assignment identity saved progress."))?;
    for row in incoming {
        let id = string(row, "SysRowID")?;
        if !guid(id) || !seen.insert(id.to_ascii_lowercase()) {
            return Err(err("An assignment row identity is invalid or repeated."));
        }
    }
    r.state["rows"]
        .as_array_mut()
        .ok_or_else(|| err("Herbert could not continue this download from its saved progress (access assignment position). Start the download again."))?
        .extend(incoming.iter().cloned());
    r.state["seen"] = json!(seen);
    let original = r.state["total"].as_u64().unwrap();
    let count = r.state["rows"].as_array().unwrap().len() as u64;
    if count > original || (incoming.is_empty() && count < original) {
        return Err(err("Assignment paging did not cover its declared count."));
    }
    if count < original {
        r.state["after"] = incoming.last().unwrap()["SysRowID"].clone();
        return surface_action(r, index);
    }
    // Raw keyset/count checks above cover every received row. Only rows the literal
    // user prefix keeps are validated and projected; others produce no diagnostics.
    let prefix = user_prefix(&r, index)?;
    let rows = r.state["rows"].as_array().unwrap();
    let records = if prefix.is_empty() {
        assignment_records(index, rows, &r.company)?
    } else {
        let kept: Vec<Value> = rows.iter().filter(|row| row["UserID"].as_str().is_some_and(|user| user.starts_with(&prefix))).cloned().collect();
        assignment_records(index, &kept, &r.company)?
    };
    finish(r, records, index)
}
fn handle_inner(r: GuestRequest) -> GuestReply {
    let index = r.state["surface"].as_u64().map(|n| n as usize).or_else(|| {
        r.cursor
            .as_ref()
            .and_then(|c| serde_json::from_str::<Value>(c).ok())
            .and_then(|v| v["surface"].as_u64())
            .map(|n| n as usize)
    });
    let independent = r.identity.is_none() && r.operation != "materialize";
    match drive(r) {
        Ok(v) => v,
        Err(d) if independent && index.is_some_and(|i| i < SURFACES.len()) => {
            GuestReply::Inventory {
                identities: vec![],
                next_cursor: next(index.unwrap()),
                authoritative: false,
                diagnostics: vec![d],
            }
        }
        Err(d) => GuestReply::Failure { diagnostic: d },
    }
}

#[cfg(test)]
mod bulk_output_tests {
    use super::*;
    #[test]
    fn access_bulk_preserves_complete_records_and_legacy_checkpoint_output() {
        let records: Vec<(Identity, Value)> = (0..37).map(|n| (Identity {
            category:"access-and-assignments".into(), kind:"AccessScope".into(), scope:Scope::Global,
            keys:BTreeMap::from([("AccessScopeID".into(), n.to_string())]),
        }, json!({"herbertSources":{"files":[{"name":"original.json","text":"é exact"}],"binaryFiles":[{"name":"scope.eas","base64":"AAH/"}]},"herbertCollectionStatus":{"completeness":"partial","diagnostics":[{"code":"fixture"}]},"unknown":{"keep":n}}))).collect();
        let mut legacy = json!({"records":records,"index":0,"next":"next-surface"});
        let mut expected = Vec::new();
        loop {
            match output(legacy).unwrap() {
                GuestReply::ReadyRecords {records,state} => { expected.extend(records); legacy=state; },
                GuestReply::Inventory {next_cursor,..} => {assert_eq!(next_cursor.as_deref(),Some("next-surface"));break;},
                other => panic!("{other:?}"),
            }
        }
        let GuestReply::InventoryData {records:actual,inventory} = output(json!({"bulk":true,"records":records,"next":"next-surface"})).unwrap() else {panic!()};
        assert_eq!(actual,expected);
        assert!(matches!(*inventory,GuestReply::Inventory{identities,next_cursor:Some(_),authoritative:true,..} if identities.len()==37));
        assert!(matches!(output(json!({"bulk":true,"records":[],"next":null})).unwrap(),GuestReply::InventoryData{records,..} if records.is_empty()));
    }
    #[test]
    fn access_bulk_requires_host_negotiation_and_survives_version_setup() {
        for negotiated in [false,true] {
            let r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"access-and-assignments".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state:json!({"_hostPreparePage":true,"_hostIndependentWorkItems":negotiated}),response:None,error:None};
            let GuestReply::Action{state,..}=handle(r) else {panic!()};
            assert_eq!(state["bulk"],negotiated);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_menu_security_is_source_with_an_unverified_object_recovery_reference() {
        let row = json!({"Company":"","SecCode":"bo.UD40.GetRows","ParentSecCode":"bo.UD40","ProductionWorker":true,"Future":"preserve","SystemFlag":false,"SysRowID":"r"});
        let out = native_records(
            0,
            &json!({"Security":[row.clone()],"ExtensionTables":[]}),
            "C",
            "",
        )
        .unwrap();
        assert_eq!(out[0].0.kind, "NativeSecuritySourceSupplement");
        assert_eq!(out[0].1["definition"]["Security"][0], row);
        assert_eq!(
            out[0].1["herbertRestore"]["contract"],
            "source-only-supplement"
        );
        assert_eq!(
            out[0].1["herbertRestore"]["relatedDefinition"]["nativeKey"]["SecCode"],
            "bo.UD40"
        );
        assert_eq!(
            out[0].1["herbertRestore"]["relatedDefinition"]["resolution"],
            "unverified-reference"
        );
        let object = json!({"Company":"Other","SecCode":"bo.Custom","ParentSecCode":"OBJECT","AllCompanies":true,"SystemFlag":false,"SysRowID":"object"});
        let root = native_records(0, &json!({"Security":[object]}), "C", "").unwrap();
        assert_eq!(
            root[0].1["herbertRestore"]["relatedDefinition"]["nativeKey"]["Company"],
            "Other"
        );
        assert_eq!(
            root[0].1["herbertRestore"]["relatedDefinition"]["nativeKey"]["SecCode"],
            "bo.Custom"
        );
        for parent in ["MENU", "COLLABORATE"] {
            let menu = json!({"Company":"C","SecCode":"M","ParentSecCode":parent,"SystemFlag":false,"SysRowID":"m"});
            let normal = native_records(0, &json!({"Security":[menu]}), "C", "").unwrap();
            assert_eq!(normal[0].0.kind, "SecurityDefinition");
            assert_eq!(normal[0].1["herbertRestore"]["contract"], "native-tableset");
        }
    }
    #[test]
    fn physical_shared_visibility_is_sufficient_without_a_display_flag() {
        for marker in [5, 10] {
            let row = json!({"Company":"Other","SecCode":"Menu","ParentSecCode":"MENU","CompanyVisibility":marker,"AllCompanies":false,"SystemFlag":false,"SysRowID":"r"});
            let out = native_records(0, &json!({"Security":[row.clone()]}), "C", "").unwrap();
            assert_eq!(out[0].1["definition"]["Security"][0], row);
            assert!(matches!(out[0].0.scope, Scope::SharedCompany { .. }));
        }
    }
    #[test]
    fn denied_version_preserves_its_cause_and_useful_scope_source() {
        let mut r = request("discover", 4);
        r.state["phase"] = json!("version");
        r.state["version"] = json!("");
        r.error = Some(Diagnostic::new(
            "ACCESS_DENIED",
            "Native version metadata denied.",
        ));
        let GuestReply::Action { state, .. } = handle(r.clone()) else {
            panic!()
        };
        r.state = state;
        r.error = None;
        r.response = Some(
            json!({"returnObj":{"AccessScope":[{"Company":"C","AccessScopeID":"A","SystemFlag":false,"SysRowID":"r"}],"AccessScopeEntity":[],"AccessScopeBOMethod":[]},"parameters":{"morePages":false}}),
        );
        let GuestReply::PreparedPage { state, .. } = handle(r) else {
            panic!()
        };
        let data = &state["records"][0][1];
        assert_eq!(data["definition"]["AccessScope"][0]["AccessScopeID"], "A");
        assert_eq!(data["herbertCollectionStatus"]["completeness"], "partial");
        assert!(
            data["herbertCollectionStatus"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "ACCESS_DENIED")
        );
        assert!(
            data["herbertSources"]["binaryFiles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn shared_key_keeps_native_owner_and_private_foreign_key_is_refused() {
        let row = json!({"Company":"Other","KeyID":"K","Name":"shared","AccessScopeID":"A","Enabled":false,"ExpiresOn":null,"AllCompanies":true,"SystemFlag":false,"SysRowID":"00000000-0000-0000-0000-000000000001","SysRevID":1});
        let out = assignment_records(7, &[row.clone()], "C").unwrap();
        assert_eq!(
            out[0].0.scope,
            Scope::SharedCompany {
                company: "C".into(),
                owner_company: "Other".into()
            }
        );
        assert_eq!(out[0].0.keys["Company"], "Other");
        assert_eq!(out[0].1["definition"]["APIKey"][0], row);
        let mut private = row;
        private["AllCompanies"] = json!(false);
        assert!(assignment_records(7, &[private], "C").is_err());
    }
    #[test]
    fn shared_scope_children_inherit_native_parent_visibility_and_preserve_owner() {
        let root = json!({"Company":"Other","AccessScopeID":"A","AllCompanies":true,"CompanyVisibility":10,"SystemFlag":false,"SysRowID":"r"});
        let entity = json!({"Company":"Other","AccessScopeID":"A","EntityType":"Service","EntityID":"Ice.BO.UserCodes","SystemFlag":false,"SysRowID":"e"});
        let method = json!({"Company":"Other","AccessScopeID":"A","EntityType":"Service","EntityID":"Ice.BO.UserCodes","MethodID":"GetRows","SystemFlag":false,"SysRowID":"m"});
        let out=native_records(4,&json!({"AccessScope":[root.clone()],"AccessScopeEntity":[entity.clone()],"AccessScopeBOMethod":[method.clone()]}),"C","5.2.100.0").unwrap();
        assert_eq!(
            out[0].0.scope,
            Scope::SharedCompany {
                company: "C".into(),
                owner_company: "Other".into()
            }
        );
        assert_eq!(out[0].1["definition"]["AccessScope"][0], root);
        assert_eq!(out[0].1["definition"]["AccessScopeEntity"][0], entity);
        assert_eq!(out[0].1["definition"]["AccessScopeBOMethod"][0], method);
        assert_eq!(
            out[0].1["herbertCollectionStatus"]["completeness"],
            "collected"
        );
    }
    #[test]
    fn external_company_assignment_preserves_full_relationship_and_selected_scope() {
        let row = json!({"UserID":"U","Company":"C","ExtCompID":"External","CurPlant":"Plant","SysRowID":"00000000-0000-0000-0000-000000000001","SysRevID":1});
        let out = assignment_records(8, &[row.clone()], "C").unwrap();
        assert_eq!(out[0].0.keys["ExtCompID"], "External");
        assert_eq!(out[0].1["definition"]["UserCompExt"][0], row);
        let mut foreign = row;
        foreign["Company"] = json!("Other");
        assert!(assignment_records(8, &[foreign], "C").is_err());
    }
    #[test]
    fn custom_method_beneath_stock_parent_survives_without_custom_roots() {
        let child = json!({"Company":"","SecCode":"bo.UD40.GetRows","ParentSecCode":"bo.UD40","SystemFlag":false,"SysRowID":"child","EntryList":"Owned","Future":"ü & <text>"});
        let result = native_records(
            1,
            &json!({"Security":[],"ObjectSecurityMethod":[child.clone()],"ExtensionTables":[]}),
            "C",
            "5.2.100.0",
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.scope, Scope::Global);
        assert_eq!(result[0].0.keys["SecCode"], "bo.UD40");
        assert_eq!(result[0].1["definition"]["Security"], json!([]));
        assert_eq!(result[0].1["definition"]["ObjectSecurityMethod"][0], child);
        assert_eq!(
            result[0].1["herbertParentContext"]["authoredParentCollected"],
            false
        );
    }
    #[test]
    fn exact_parentsec_code_and_scope_group_methods() {
        let root = json!({"Company":"C","SecCode":"bo.X","SystemFlag":false,"SysRowID":"r"});
        let children = json!([{"Company":"C","SecCode":"bo.X.Read","ParentSecCode":"bo.X","SystemFlag":false,"SysRowID":"a"},{"Company":"","SecCode":"bo.X.Read","ParentSecCode":"bo.X","SystemFlag":false,"SysRowID":"b"}]);
        let result = native_records(
            1,
            &json!({"Security":[root.clone()],"ObjectSecurityMethod":children}),
            "C",
            "",
        )
        .unwrap();
        assert_eq!(result.len(), 2);
        let owned = result
            .iter()
            .find(|(id, _)| id.scope == scope("C"))
            .unwrap();
        assert_eq!(owned.1["definition"]["Security"][0], root);
        assert_eq!(
            owned.1["definition"]["ObjectSecurityMethod"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn foreign_system_duplicate_and_untyped_rows_are_rejected() {
        let base = json!({"Company":"C","SecGroupCode":"G","SystemFlag":false,"SysRowID":"r"});
        let mut foreign = base.clone();
        foreign["Company"] = json!("Foreign");
        for bad in [foreign, json!({})] {
            assert!(native_records(2, &json!({"SecGroup":[bad]}), "C", "").is_err());
        }
        let mut stock = base.clone();
        stock["SystemFlag"] = json!(true);
        assert!(native_records(2, &json!({"SecGroup":[stock]}), "C", "").is_err());
        assert!(native_records(2, &json!({"SecGroup":[base.clone(),base]}), "C", "").is_err());
    }

    #[test]
    fn access_families_skip_only_unacquired_native_surfaces(){for family in ["all","definitions","userAssignments","apiKeyAssignments"]{for index in 0..9{let mut r=request("discover",index);r.state=json!({"_pluginSettings":{"accessFamily":family,"userIdPrefix":""}});r.cursor=Some(json!({"surface":index}).to_string());let reply=handle(r);let wanted=family=="all"||family==if index<5{"definitions"}else if index==7{"apiKeyAssignments"}else{"userAssignments"};if wanted{assert!(matches!(reply,GuestReply::Action{state,..}if state["_pluginSettings"]["accessFamily"]==family));}else{assert!(matches!(reply,GuestReply::Inventory{authoritative:true,..}));}}}}
    #[test]
    fn literal_user_prefix_preserves_all_three_complete_assignment_projections(){
        for index in [5,6,8]{let rows:Vec<_>=[" Ω%_ '","Other"].into_iter().enumerate().map(|(i,name)|{let mut row=if index==5{user(name,&format!("00000000-0000-0000-0000-{:012}",i+1))}else if index==6{json!({"UserID":name,"Company":"C","CurPlant":"M","PlantList":"M~Other","SysRowID":format!("00000000-0000-0000-0000-{:012}",i+1),"SysRevID":1})}else{json!({"UserID":name,"Company":"C","ExtCompID":"OutsidePrefix","CurPlant":"M","SysRowID":format!("00000000-0000-0000-0000-{:012}",i+1),"SysRevID":1})};row["SysRevID"]=json!(9007199254740000i64);row}).collect();let all=assignment_records(index,&rows,"C").unwrap();for prefix in [""," Ω%_ '","Ω","other"]{let settings=json!({"accessFamily":"userAssignments","userIdPrefix":prefix});let (actual,errors)=select_records(all.clone(),&json!({"_pluginSettings":settings})).unwrap();assert!(errors.is_empty());assert_eq!(actual,all.iter().filter(|(id,_)|id.keys["UserID"].starts_with(prefix)).cloned().collect::<Vec<_>>());}}
    }
    #[test]
    fn assignment_keyset_pushes_user_stem_advances_past_excluded_superset_page_and_skips_excluded_validation(){
        // "a" models the server's case-insensitive starts-with superset for the literal prefix "A".
        let mut r=request("discover",5);r.state["_pluginSettings"]=json!({"accessFamily":"userAssignments","userIdPrefix":"A"});r.response=Some(json!({"value":[user("a","00000000-0000-0000-0000-000000000001")],"@odata.count":2}));let GuestReply::Action{state,action:HostAction::NativeEntityPage{query,..}}=handle(r.clone())else{panic!()};assert_eq!(query["$filter"],"startswith(UserID,'A') and SysRowID gt 00000000-0000-0000-0000-000000000001");assert_eq!(state["_pluginSettings"]["userIdPrefix"],"A");r.state=state;r.response=Some(json!({"value":[user("A","00000000-0000-0000-0000-000000000002")],"@odata.count":1}));let GuestReply::PreparedPage{state,next_state,..}=handle(r.clone())else{panic!()};assert!(state.get("_hostWorkItems").is_none());assert_eq!(state["records"].as_array().unwrap().len(),1);assert_eq!(state["records"][0][0]["keys"]["UserID"],"A");assert_eq!(next_state["_pluginSettings"]["userIdPrefix"],"A");
        // An unselected row is not validated and produces no diagnostics; a selected invalid row still fails.
        for (name,selected) in [("a",false),("A",true)]{let mut bad=user(name,"00000000-0000-0000-0000-000000000002");bad["Password"]=json!("EXCLUDED_PRIVATE");r.response=Some(json!({"value":[bad],"@odata.count":1}));match handle(r.clone()){GuestReply::PreparedPage{state,..} if !selected=>{assert_eq!(state["records"],json!([]));assert!(state.get("diagnostics").is_none());},GuestReply::Inventory{authoritative:false,diagnostics,..} if selected=>assert!(diagnostics.iter().any(|d|d.code=="ACCESS_GRAPH_INVALID")),reply=>panic!("{name}: {reply:?}")}}
    }
    #[test]
    fn user_prefix_pushes_startswith_only_onto_user_assignment_reads_and_not_exact_acquisition(){
        for (prefix,stem) in [("O'Brien",Some("O''Brien")),("ab_c",Some("ab")),("[x",None),("%",None),("",None)]{for index in 0..9{
            let mut r=request("discover",index);r.state["phase"]=json!("version");r.state["_pluginSettings"]=json!({"accessFamily":"all","userIdPrefix":prefix});
            let GuestReply::Action{action,..}=handle(r)else{panic!("{index}")};
            if index<5{let text=json!(action).to_string();assert!(!text.contains("UserID")&&!text.contains("startswith"),"{index}/{prefix}");continue;}
            let HostAction::NativeEntityPage{query,..}=action else{panic!("{index}")};
            let mut filters:Vec<String>=vec![];if matches!(index,6|8){filters.push("Company eq 'C'".into());}
            if let Some(stem)=stem{if matches!(index,5|6|8){filters.push(format!("startswith(UserID,'{stem}')"));}}
            assert_eq!(query.get("$filter").cloned(),(!filters.is_empty()).then(||filters.join(" and ")),"{index}/{prefix}");
        }}
        for index in [5,6,8]{let(kind,_)=SURFACES[index];let mut r=request("acquire",index);r.state["phase"]=json!("version");r.identity=Some(Identity{category:"access-and-assignments".into(),kind:kind.into(),scope:if index==5{Scope::Global}else{Scope::Company{company:"C".into()}},keys:BTreeMap::from([("UserID".into(),"B".into())])});let baseline=serde_json::to_value(handle(r.clone())).unwrap();r.state["_pluginSettings"]=json!({"accessFamily":"definitions","userIdPrefix":"A"});let reply=serde_json::to_value(handle(r)).unwrap();assert_eq!(baseline["type"],"action");assert_eq!(reply["action"],baseline["action"]);assert!(!reply["action"].to_string().contains("startswith"));}
    }
    #[test]
    fn excluded_partial_diagnostics_and_scope_version_survive_empty_bulk_and_legacy_output(){
        let id=Identity{category:"access-and-assignments".into(),kind:"NativeAccessSupplementary".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::from([("Service".into(),"Ice.BO.SecuritySvc".into())])};let record=(id,complete("Ice.BO.SecuritySvc","NativeAccessSupplementary",json!({"extra":[{"source":"EXCLUDED_PRIVATE"}]}),Value::Null,"",vec![Diagnostic::new("ACQUIRED","unknown relationship")]));for bulk in [false,true]{let mut r=request("discover",0);r.state["bulk"]=json!(bulk);r.state["_pluginSettings"]=json!({"accessFamily":"userAssignments","userIdPrefix":"Missing"});let GuestReply::PreparedPage{state,next_state,..}=selected_reply(finish(r.clone(),vec![record.clone()],0).unwrap(),&r.state).unwrap()else{panic!()};assert_eq!(state["records"],json!([]));assert!(state.get("_hostWorkItems").is_none());assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);r.operation="materialize".into();r.state=state;match handle(r){GuestReply::Inventory{authoritative:false,diagnostics,..}=>assert!(!diagnostics.is_empty()),GuestReply::InventoryData{records,inventory}=>{assert!(records.is_empty());assert!(matches!(*inventory,GuestReply::Inventory{authoritative:false,diagnostics,..}if !diagnostics.is_empty()));},reply=>panic!("{reply:?}")}}
        let mut r=request("discover",4);r.state["versionDiagnostic"]=json!(Diagnostic::new("VERSION_DENIED","original cause"));r.state["_pluginSettings"]=json!({"accessFamily":"userAssignments","userIdPrefix":"Missing"});r.response=Some(json!({"returnObj":{"AccessScope":[],"AccessScopeEntity":[],"AccessScopeBOMethod":[]},"parameters":{"morePages":false}}));let GuestReply::PreparedPage{state,..}=handle(r)else{panic!()};assert_eq!(state["diagnostics"][0]["code"],"VERSION_DENIED");}
    #[test]
    fn exact_access_ignores_invalid_discovery_controls_and_nonuser_families_ignore_prefix(){for index in 0..9{let (kind,_)=SURFACES[index];let scope=if index==5{Scope::Global}else{Scope::Company{company:"C".into()}};let id=Identity{category:"access-and-assignments".into(),kind:kind.into(),scope,keys:BTreeMap::from([("Company".into(),if index==5{"".into()}else{"C".into()})])};let mut r=request("acquire",index);r.identity=Some(id);r.state=json!({});let baseline=handle(r.clone());r.state["_pluginSettings"]=json!({"accessFamily":false,"userIdPrefix":false});assert_eq!(json!(handle(r)),json!(baseline));}for kind in ["SecurityDefinition","ObjectSecurity","SecurityGroup","ColumnSecurity","AccessScope","APIKeyScopeAssignment"]{let id=Identity{category:"access-and-assignments".into(),kind:kind.into(),scope:Scope::Global,keys:BTreeMap::new()};assert!(selected_identity(&id,&json!({"_pluginSettings":{"accessFamily":"all","userIdPrefix":"no match"}})).unwrap());}}
    fn request(operation: &str, index: usize) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: operation.into(),
            category: "access-and-assignments".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: None,
            state: json!({"phase":"rows","surface":index,"prepare":true,"rows":[],"seen":[],"version":"5.2.100.0"}),
            response: None,
            error: None,
        }
    }
    fn user(name: &str, guid: &str) -> Value {
        json!({"UserID":name,"GroupList":"G","CompList":"C~Other","SecurityMgr":false,"GlbCompSM":false,"AccessScopeID":"A","UserDisabled":false,"SysRowID":guid,"SysRevID":1})
    }
    #[test]
    fn assignment_projection_rejects_credential_or_personal_expansion() {
        let row = user("U", "00000000-0000-0000-0000-000000000001");
        let result = assignment_records(5, &[row.clone()], "C").unwrap();
        assert_eq!(result[0].1["definition"]["UserFile"][0], row);
        let mut secret = row;
        secret["Password"] = json!("should never be downloaded");
        assert!(assignment_records(5, &[secret], "C").is_err());
    }
    #[test]
    fn short_server_page_continues_after_received_key_without_offset_skip() {
        let a = "00000000-0000-0000-0000-000000000001";
        let b = "00000000-0000-0000-0000-000000000002";
        let c = "00000000-0000-0000-0000-000000000003";
        let mut r = request("discover", 5);
        r.response = Some(json!({"value":[user("A",a)],"@odata.count":3}));
        let GuestReply::Action {
            action:
                HostAction::NativeEntityPage {
                    query,
                    page,
                    page_size,
                    ..
                },
            state,
        } = handle(r.clone())
        else {
            panic!("A short server page must continue")
        };
        assert_eq!(page, 1);
        assert_eq!(page_size, i32::MAX as u32);
        assert_eq!(query["$filter"], format!("SysRowID gt {a}"));
        assert!(!query.contains_key("$skip"));
        r.state = state;
        r.response = Some(json!({"value":[user("B",b),user("C",c)],"@odata.count":2}));
        let GuestReply::PreparedPage { state, .. } = handle(r) else {
            panic!("All three native rows must materialize")
        };
        assert_eq!(state["records"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn duplicate_or_changed_keyset_counts_report_incomplete_discovery() {
        let a = "00000000-0000-0000-0000-000000000001";
        let mut r = request("discover", 5);
        r.state["rows"] = json!([user("A", a)]);
        r.state["seen"] = json!([a]);
        r.state["total"] = json!(3);
        for response in [
            json!({"value":[user("A",a)],"@odata.count":2}),
            json!({"value":[],"@odata.count":2}),
            json!({"value":[],"@odata.count":3}),
        ] {
            r.response = Some(response);
            assert!(matches!(
                handle(r.clone()),
                GuestReply::Inventory {
                    authoritative: false,
                    next_cursor: Some(_),
                    ..
                }
            ));
        }
    }
    #[test]
    fn exact_denial_preserves_diagnostic_discovery_continues_independently() {
        let mut r = request("discover", 2);
        let d = Diagnostic::new("ACCESS_DENIED", "Native denied this family.");
        r.error = Some(d.clone());
        let GuestReply::Inventory {
            authoritative,
            diagnostics,
            next_cursor,
            ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert!(!authoritative);
        assert_eq!(diagnostics[0].code, d.code);
        assert_eq!(next_cursor, next(2));
        r.operation = "acquire".into();
        r.identity = Some(
            identity(
                "SecurityGroup",
                &json!({"Company":"C","SecGroupCode":"G"}),
                &["Company", "SecGroupCode"],
                "C",
            )
            .unwrap(),
        );
        assert!(matches!(handle(r),GuestReply::Failure{diagnostic}if diagnostic.code==d.code));
    }
    #[test]
    fn missing_exact_target_is_failed_without_a_discovery_cursor() {
        let mut r = request("acquire", 2);
        r.identity = Some(
            identity(
                "SecurityGroup",
                &json!({"Company":"C","SecGroupCode":"G"}),
                &["Company", "SecGroupCode"],
                "C",
            )
            .unwrap(),
        );
        r.response = Some(json!({"returnObj":{"SecGroup":[]},"parameters":{"morePages":false}}));
        assert!(
            matches!(handle(r),GuestReply::Failure{diagnostic}if diagnostic.code=="ACCESS_NOT_FOUND")
        );
    }
    #[test]
    fn unknown_native_data_survives_even_when_known_roots_are_empty() {
        let out = native_records(
            2,
            &json!({"SecGroup":[],"NewNativeTable":[{"Something":"preserve"}]}),
            "C",
            "",
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].1["herbertCollectionStatus"]["completeness"],
            "partial"
        );
        assert_eq!(
            out[0].1["herbertRestore"]["contract"],
            "source-only-unverified-relations"
        );
        assert_eq!(
            out[0].1["definition"]["herbertUnassociatedNativeData"]["NewNativeTable"][0]["Something"],
            "preserve"
        );
    }
    #[test]
    fn unknown_extension_data_stays_source_beside_a_supported_menu() {
        let row = json!({"Company":"C","SecCode":"M","ParentSecCode":"MENU","SystemFlag":false,"SysRowID":"r"});
        let extra = json!([{"Unknown":"ü & <text>","Nested":{"Keep":true}}]);
        let out = native_records(0, &json!({"Security":[row.clone()],"ExtensionTables":extra.clone(),"NewNativeTable":extra.clone()}), "C", "").unwrap();
        assert_eq!(out[0].1["definition"]["Security"][0], row);
        assert_eq!(out[0].1["definition"]["ExtensionTables"], extra);
        assert_eq!(
            out[0].1["definition"]["herbertUnassociatedNativeData"]["NewNativeTable"],
            extra
        );
        assert_eq!(
            out[0].1["herbertCollectionStatus"]["completeness"],
            "partial"
        );
        assert!(
            out[0].1["herbertRestore"]["instruction"]
                .as_str()
                .unwrap()
                .contains("do not replay")
        );
    }
    #[test]
    fn native_column_table_security_allows_empty_native_column_key() {
        let row = json!({"Company":"C","DatasourceType":"","SchemaName":"Ice","TableName":"UD40","ColumnName":"","SysRowID":"column","ReadAllowList":"*"});
        let out = native_records(3, &json!({"SecColumn":[row.clone()]}), "C", "").unwrap();
        assert_eq!(out[0].1["definition"]["SecColumn"][0], row);
    }
}

pub fn handle(r:GuestRequest)->GuestReply{
    if r.operation=="acquire"||r.identity.is_some(){return handle_inner(r);}
    let mut from=r.state.clone();if let Err(d)=selection(&from){return GuestReply::Failure{diagnostic:d};}
    if r.operation=="discover"&&from["phase"].is_null()&&r.response.is_none()&&r.error.is_none(){let i=if let Some(cursor)=&r.cursor{serde_json::from_str::<Value>(cursor).ok().and_then(|v|(v.as_object().is_some_and(|m|m.len()==1)).then(||v["surface"].as_u64()).flatten()).map(|n|n as usize)}else{Some(0)};if let Some(i)=i.filter(|i|*i<SURFACES.len()){if matches!(selected_surface(i,&from),Ok(false)){return selected_reply(GuestReply::Inventory{identities:vec![],next_cursor:next(i),authoritative:true,diagnostics:vec![]},&from).unwrap_or_else(|diagnostic|GuestReply::Failure{diagnostic});}}}
    if from["surface"]==4&&!from["versionDiagnostic"].is_null(){let errors=serde_json::from_value::<Diagnostic>(from["versionDiagnostic"].clone());match errors{Ok(d)=>from=selection_settings(from.clone(),&Value::Null,vec![d]),Err(_)=>return GuestReply::Failure{diagnostic:err("Invalid acquired version diagnostic.")}}}
    match selected_reply(handle_inner(r),&from){Ok(reply)=>reply,Err(diagnostic)=>GuestReply::Failure{diagnostic}}
}
