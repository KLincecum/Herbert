//! Custom resources direct resource collection. Read whole exposed streams, then construct locally.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn err(s: &str) -> Diagnostic {
    Diagnostic::new("CUSTOM_RESOURCE_INVALID", s)
}
fn family(state:&Value)->Result<&str,Diagnostic>{match state.pointer("/_pluginSettings/resourceFamily"){None=>Ok("all"),Some(Value::String(value)) if matches!(value.as_str(),"all"|"themes"|"translations"|"images"|"fileStore")=>Ok(value),_=>Err(err("Select a declared resource family."))}}
fn stream_family(stream:u64)->&'static str {match stream{0|4=>"themes",1=>"translations",2=>"images",3=>"fileStore",_=>""}}
fn selected(identity:&Identity,family:&str)->bool {family=="all"||family==match identity.kind.as_str(){"Theme"|"StandaloneThemeResource"=>"themes","TranslationLibrary"=>"translations","ImageLibrary"=>"images","FileStoreCustomizationResource"=>"fileStore",_=>""}}
fn carry_state(mut state:Value,from:&Value,diagnostics:bool)->Value{
    if !state.is_object(){state=json!({});}
    if let Some(settings)=from.get("_pluginSettings"){state["_pluginSettings"]=settings.clone();}
    if diagnostics {if let Some(errors)=from["diagnostics"].as_array(){if !state["diagnostics"].is_array(){state["diagnostics"]=json!([]);}for error in errors{let output=state["diagnostics"].as_array_mut().unwrap();if !output.contains(error){output.push(error.clone());}}}}
    state
}
fn selected_reply(reply:GuestReply,from:&Value)->Result<GuestReply,Diagnostic>{
    let family=family(from)?;
    let filter=|records:&mut Vec<(Identity,Value)>|records.retain(|(identity,_)|selected(identity,family));
    let filter_state=|mut state:Value|->Result<Value,Diagnostic>{if state["records"].is_array(){let mut records:Vec<(Identity,Value)>=serde_json::from_value(state["records"].take()).map_err(|_|err("Invalid prepared resource owners."))?;filter(&mut records);state["records"]=json!(records);state["index"]=json!(0);}Ok(state)};
    Ok(match reply {
        GuestReply::PreparedPage {mut state,next_cursor,next_state}=>{
            if let Some(items)=state.get_mut("_hostWorkItems").and_then(Value::as_array_mut){if items.is_empty()&&from["diagnostics"].as_array().is_some_and(|errors|!errors.is_empty()){items.push(json!({"phase":"output","records":[],"index":0,"next":null}));}for (i,item) in items.iter_mut().enumerate(){*item=carry_state(filter_state(item.take())?,from,i==0);}}
            state=carry_state(filter_state(state)?,from,true);GuestReply::PreparedPage {state,next_cursor,next_state:carry_state(next_state,from,false)}
        },
        GuestReply::Action {action,state}=>GuestReply::Action {action,state:carry_state(state,from,true)},
        GuestReply::ReadyRecords {mut records,state}=>{filter(&mut records);GuestReply::ReadyRecords {records,state:carry_state(state,from,true)}},
        GuestReply::InventoryData {mut records,inventory}=>{filter(&mut records);GuestReply::InventoryData {records,inventory}},
        GuestReply::Inventory {mut identities,next_cursor,mut authoritative,mut diagnostics}=>{identities.retain(|identity|selected(identity,family));if let Some(errors)=from["diagnostics"].as_array(){for error in errors {let diagnostic:Diagnostic=serde_json::from_value(error.clone()).map_err(|_|err("Invalid acquired resource diagnostic."))?;if !diagnostics.iter().any(|current|json!(current)==json!(diagnostic)){diagnostics.push(diagnostic);}}}authoritative&=diagnostics.is_empty();GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics}},
        other=>other,
    })
}
pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.BO.ThemeSvc", "GetRows"),
        ("Ice.BO.LangNameSvc", "GetRows"),
        ("Ice.BO.LangTranSvc", "GetRows"),
        ("Ice.BO.LangOrgSvc", "GetRows"),
        ("Erp.BO.ImageSvc", "GetRows"),
        ("Erp.BO.ImageLayerSvc", "GetRows"),
        ("Erp.BO.ImageCategorySvc", "GetRows"),
        ("Ice.BO.BAQDesignerSvc", "GetFieldList"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
    ]
    .into_iter()
    .map(|(a, b)| (a.into(), b.into()))
    .collect()
}
pub fn policies() -> Vec<NativeResponsePolicy> {
    let mut p = crate::response_policy::for_methods(&methods());
    for policy in &mut p {
        // Linked original texts, language metadata and custom overrides can be
        // system-marked. Explicit source predicates and local ownership checks
        // select them; blanket target-artifact filtering would destroy links.
        policy.retention_rules.clear();
        if let Some(t) = &mut policy.tables {
            t.unscoped_tables.push("Results".into());
        }
    }
    p
}
fn id(kind: &str, row: &Value, keys: &[&str]) -> Result<Identity, Diagnostic> {
    let company = row
        .get("Company")
        .map(|v| v.as_str().ok_or_else(|| err("Epicor did not say which company these records belong to, so they were not saved.")))
        .transpose()?
        .unwrap_or("");
    let mut result = BTreeMap::new();
    for k in keys {
        let v = row
            .get(*k)
            .filter(|v| v.is_string() || v.is_number())
            .ok_or_else(|| err("Missing Epicor resource key."))?;
        result.insert(
            (*k).into(),
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string()),
        );
    }
    Ok(Identity {
        category: "custom-resources".into(),
        kind: kind.into(),
        scope: if company.is_empty()
            || (kind == "FileStoreCustomizationResource" && company == "All")
        {
            Scope::Global
        } else {
            Scope::Company {
                company: company.into(),
            }
        },
        keys: result,
    })
}
fn scoped(row: &Value, company: &str) -> Result<(), Diagnostic> {
    if let Some(c) = row.get("Company") {
        if c != "" && c != company && !(c == "All" && row.get("RelatedToTable").is_some()) {
            return Err(err("Resource belongs to another company."));
        }
    }
    Ok(())
}
fn rows<'a>(v: &'a Value, t: &str) -> Result<&'a Vec<Value>, Diagnostic> {
    v[t].as_array()
        .ok_or_else(|| err("Epicor resource response omitted a required table."))
}
fn binary(name: String, encoded: &str) -> Result<Value, Diagnostic> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| err("Invalid original resource bytes."))?;
    Ok(
        json!({"name":name,"base64":encoded,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}),
    )
}
// Encode filesystem-unsafe characters without discarding the readable native name.
// Original paths remain exact metadata; only their leaf is used for local files.
fn readable_component(value: &str) -> String {
    let mut name = String::new();
    for c in value.chars() {
        if c.is_control() || "<>:\"/\\|?*%".contains(c) {
            for byte in c.to_string().as_bytes() {
                name.push_str(&format!("%{byte:02X}"));
            }
        } else {
            name.push(c);
        }
    }
    while name.ends_with(['.', ' ']) {
        let c = name.pop().unwrap();
        name.push_str(if c == '.' { "%2E" } else { "%20" });
    }
    if name.is_empty() {
        return "Unnamed".into();
    }
    let stem = name.split('.').next().unwrap().to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
    {
        name = format!("%{:02X}{}", name.as_bytes()[0], &name[1..]);
    }
    name
}
fn resource_filename(original: &str, file_type: &str, encoded: &str) -> Result<String, Diagnostic> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| err("Invalid original resource bytes."))?;
    // Recognized signatures take precedence over misleading or missing metadata.
    let detected = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else if bytes.starts_with(b"II\x2a\0") || bytes.starts_with(b"MM\0\x2a") {
        Some("tif")
    } else if bytes.starts_with(b"\0\0\x01\0") {
        Some("ico")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else {
        None
    };
    let leaf = original.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name = readable_component(leaf);
    let ext = leaf
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .filter(|ext| !ext.is_empty());
    let declared = file_type.trim().trim_start_matches('.');
    let extension = detected.or_else(|| ext).or_else(|| {
        (!declared.is_empty() && declared.chars().all(|c| c.is_ascii_alphanumeric()))
            .then_some(declared)
    });
    if let Some(extension) = extension {
        if let Some(old) = ext {
            let matches = old.eq_ignore_ascii_case(extension)
                || (extension == "jpg" && old.eq_ignore_ascii_case("jpeg"))
                || (extension == "tif" && old.eq_ignore_ascii_case("tiff"));
            if !matches {
                name = format!(
                    "{}.{}",
                    readable_component(leaf.rsplit_once('.').unwrap().0),
                    extension
                );
            }
        } else {
            name.push('.');
            name.push_str(extension);
        }
    }
    Ok(name)
}
fn safe_id(row: &Value) -> Result<&str, Diagnostic> {
    row["SysRowID"]
        .as_str()
        .filter(|s| {
            s.len() == 36
                && s.bytes().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == b'-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
        })
        .ok_or_else(|| err("Resource has no Epicor GUID."))
}
/// Map each lowercase file ID to its (image, role) uses, in image order then role order.
fn image_index(images: &[Value], keys: &[&str]) -> BTreeMap<String, Vec<(usize, usize)>> {
    let mut index: BTreeMap<String, Vec<(usize, usize)>> = BTreeMap::new();
    for (i, image) in images.iter().enumerate() {
        for (role, key) in keys.iter().enumerate() {
            if let Some(id) = image[*key].as_str() {
                index.entry(id.to_ascii_lowercase()).or_default().push((i, role));
            }
        }
    }
    index
}
/// Group translations by (Company, LangNameID, OrgTextID) once: the first blank-program row and
/// whether any row names a program. Scalar JSON text is equal exactly when the values are equal.
fn sibling_index(translations: &[Value]) -> BTreeMap<String, (Option<usize>, bool)> {
    let mut index: BTreeMap<String, (Option<usize>, bool)> = BTreeMap::new();
    for (i, row) in translations.iter().enumerate() {
        let entry = index.entry(sibling_key(row)).or_insert((None, false));
        if row["ProgramID"] == "" {
            entry.0.get_or_insert(i);
        } else {
            entry.1 = true;
        }
    }
    index
}
fn sibling_key(row: &Value) -> String {
    json!([row["Company"], row["LangNameID"], row["OrgTextID"]]).to_string()
}
/// First row per (Company, OrgTextID), optionally only rows that name a program.
fn first_by_text<'a>(rows: &'a [Value], program: bool) -> BTreeMap<String, &'a Value> {
    let mut index = BTreeMap::new();
    for row in rows {
        if !program || row["ProgramID"] != "" {
            index.entry(json!([row["Company"], row["OrgTextID"]]).to_string()).or_insert(row);
        }
    }
    index
}
fn recovery(ds: &Value) -> Value {
    let mut ds = ds.clone();
    for table in ds.as_object_mut().into_iter().flatten().map(|(_, v)| v) {
        for row in table.as_array_mut().into_iter().flatten() {
            row["RowMod"] = json!("A");
            if row.get("SysRowID").is_some() {
                row["SysRowID"] = json!("00000000-0000-0000-0000-000000000000");
            }
            if row.get("SysRevID").is_some() {
                row["SysRevID"] = json!(0);
            }
        }
    }
    json!({"ds":ds})
}
fn complete(mut data: Value, files: Vec<Value>, binary_files: Vec<Value>) -> Value {
    data["herbertSources"] = json!({"files":files,"binaryFiles":binary_files});
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] = json!({"completeness":"collected","diagnostics":[]});
    data
}
fn finish(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    prepare: bool,
    independent: bool,
) -> GuestReply {
    if prepare && independent {
        let items: Vec<_> = records
            .into_iter()
            .map(|r| json!({"phase":"output","records":[r],"index":0,"next":null}))
            .collect();
        return GuestReply::PreparedPage {
            state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":next}),
            next_cursor: next,
            next_state: json!({}),
        };
    }
    let state = json!({"phase":"output","records":records,"index":0,"next":next});
    if prepare {
        GuestReply::PreparedPage {
            state,
            next_cursor: next,
            next_state: json!({}),
        }
    } else {
        output(state)
    }
}
fn output(mut s: Value) -> GuestReply {
    let i = s["index"].as_u64().unwrap_or(0) as usize;
    if i == s["records"].as_array().map_or(0, Vec::len) {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: s["next"].as_str().map(str::to_owned),
            authoritative: s["diagnostics"].as_array().is_none_or(Vec::is_empty),
            diagnostics: serde_json::from_value(s["diagnostics"].clone()).unwrap_or_default(),
        };
    }
    let record: (Identity, Value) = match serde_json::from_value(s["records"][i].take()) {
        Ok(v) => v,
        Err(_) => {
            return GuestReply::Failure {
                diagnostic: err("Invalid prepared resource output."),
            };
        }
    };
    s["index"] = json!(i + 1);
    GuestReply::ReadyRecords {
        records: vec![record],
        state: s,
    }
}
fn query(
    r: &GuestRequest,
    phase: &str,
    schema: &str,
    table: &str,
    fields: Vec<QueryField>,
    criteria: Vec<(&str, Value)>,
    mut state: Value,
) -> Result<GuestReply, Diagnostic> {
    let mut parameters = BTreeMap::new();
    let mut filters = vec![];
    for (field, value) in criteria {
        let p = format!("p{}", parameters.len());
        filters.push(QueryCriterion {
            table: "R".into(),
            field: field.into(),
            operator: "=".into(),
            parameter: p.clone(),
        });
        parameters.insert(p, value);
    }
    state["phase"] = json!(phase);
    paged_query(
        r,
        QuerySpec {
            tables: vec![QueryTable {
                schema: schema.into(),
                table: table.into(),
                alias: "R".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields,
            joins: vec![],
            criteria: filters,
            parameters,
            order: vec![],
            page_size: None,
            page: 1,
        },
        state,
    )
}
/// The request's page size (the host's Fallback page size setting) bounds every group read: native
/// GetRows pages and resource queries. When the setting is cleared, the host's documented default
/// (OperatingSettings, 500 from its dictionary measurements) still pages every read.
fn size(r: &GuestRequest) -> Result<Option<u32>, Diagnostic> {
    match r.page_size.or(OperatingSettings::default().page_size) {
        None => Ok(None),
        Some(n) if n > 0 && n <= i32::MAX as u32 => Ok(Some(n)),
        Some(_) => Err(err("The page size must be a whole number from 1 to 2147483647.")),
    }
}
/// Resource queries read keyset pages in SysRowID order (the host query builder refuses offset
/// paging). A full page means another may follow; drive() accumulates the pages before the phase.
fn paged_query(r: &GuestRequest, mut spec: QuerySpec, mut state: Value) -> Result<GuestReply, Diagnostic> {
    spec.page_size = size(r)?;
    spec.page = 1;
    spec.order = vec![QueryOrder { field: "SysRowID".into(), descending: false }];
    state["query"] = json!(spec);
    state["queryRows"] = json!([]);
    state["queryAfter"] = Value::Null;
    Ok(query_page(spec, state))
}
fn query_page(mut spec: QuerySpec, state: Value) -> GuestReply {
    if let Some(after) = state["queryAfter"].as_str() {
        spec.criteria.push(QueryCriterion {table:"R".into(),field:"SysRowID".into(),operator:">".into(),parameter:"after".into()});
        spec.parameters.insert("after".into(), json!(after));
    }
    GuestReply::Action { action: HostAction::SharedQuery { query: spec }, state }
}
fn all() -> Vec<QueryField> {
    vec![QueryField {
        table: "R".into(),
        field: "*".into(),
        alias: "*".into(),
        expression: None,
    }]
}
fn standalone_theme_metadata(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let mut fields: Vec<_> = [
        "ThemeID",
        "ResourceType",
        "ResourceName",
        "SysRowID",
        "SysRevID",
        "SystemFlag",
    ]
    .into_iter()
    .map(|f| QueryField {
        table: "R".into(),
        field: f.into(),
        alias: f.into(),
        expression: None,
    })
    .collect();
    for (f, name) in [
        ("ThemeID", "OwnerThemeID"),
        ("SystemFlag", "OwnerSystemFlag"),
        ("ThemeDesc", "OwnerThemeDesc"),
    ] {
        fields.push(QueryField {
            table: "Owner".into(),
            field: f.into(),
            alias: name.into(),
            expression: None,
        });
    }
    r.state["phase"] = json!("standalone-theme-metadata");
    let state = r.state.clone();
    paged_query(
        &r,
        QuerySpec {
                tables: vec![
                    QueryTable {
                        schema: "Ice".into(),
                        table: "ThemeResource".into(),
                        alias: "R".into(),
                        company_field: None,
                        metadata: vec![],
                    },
                    QueryTable {
                        schema: "Ice".into(),
                        table: "Theme".into(),
                        alias: "Owner".into(),
                        company_field: None,
                        metadata: vec![],
                    },
                ],
                fields,
                joins: vec![QueryJoin {
                    left_table: "R".into(),
                    left_field: "ThemeID".into(),
                    right_table: "Owner".into(),
                    right_field: "ThemeID".into(),
                    kind: "LeftOuter".into(),
                }],
                criteria: vec![QueryCriterion {
                    table: "R".into(),
                    field: "SystemFlag".into(),
                    operator: "=".into(),
                    parameter: "custom".into(),
                }],
                parameters: BTreeMap::from([("custom".into(), json!(false))]),
                order: vec![],
                page_size: None,
                page: 1,
        },
        state,
    )
}
fn action(mut r: GuestRequest, phase: &str, service: &str, args: Value) -> GuestReply {
    r.state["phase"] = json!(phase);
    crate::native(service, "GetRows", args, false, r.state)
}
fn response(r: &GuestRequest) -> Result<&Value, Diagnostic> {
    if let Some(d) = &r.error {
        return Err(d.clone());
    }
    let v = r
        .response
        .as_ref()
        .ok_or_else(|| err("Epicor's response did not include the resource data. Nothing was saved for this group."))?;
    // Native group reads are paged; each page must state whether another follows.
    let phase = r.state["phase"].as_str().unwrap_or("");
    if (phase == "translations" || group(phase).is_some()) && v["parameters"]["morePages"].as_bool().is_none() {
        return Err(err("The Epicor resource page did not state whether more pages follow."));
    }
    Ok(v)
}
/// Native GetRows group reads: service and the tables kept from each page, primary table first.
/// Pages accumulate per table in state.groupRows; the phase then sees one combined returnObj.
fn group(phase: &str) -> Option<(&'static str, &'static [&'static str])> {
    let read: (&'static str, &'static [&'static str]) = match phase {
        "themes" => ("Ice.BO.ThemeSvc", &["Theme", "ThemeResource"]),
        "languages" => ("Ice.BO.LangNameSvc", &["LangName"]),
        "originals" => ("Ice.BO.LangOrgSvc", &["LangOrg"]),
        "images" => ("Erp.BO.ImageSvc", &["Image"]),
        "layers" => ("Erp.BO.ImageLayerSvc", &["ImageLayerHeader", "ImageLayerDetail"]),
        "categories" => ("Erp.BO.ImageCategorySvc", &["ImageCategory", "ImageSubCategory"]),
        _ => return None,
    };
    Some(read)
}
fn page_args(r: &GuestRequest, mut args: Value, page: u64) -> Result<Value, Diagnostic> {
    let size = size(r)?;
    if page == 0 || page > i32::MAX as u64 {
        return Err(err("Herbert could not continue this download from its saved progress (resource page). Start the download again."));
    }
    // Zero is native GetRows' documented "no maximum".
    args["pageSize"] = json!(size.unwrap_or(0));
    args["absolutePage"] = json!(page);
    Ok(args)
}
fn group_action(mut r: GuestRequest, phase: &str, args: Value) -> Result<GuestReply, Diagnostic> {
    let (service, _) = group(phase).unwrap();
    let first = page_args(&r, args.clone(), 1)?;
    r.state["groupArgs"] = args;
    r.state["groupPage"] = json!(1);
    r.state["groupRows"] = json!({});
    Ok(action(r, phase, service, first))
}
/// LangTran is read in pages of the request's page size like every other group read.
fn translation_page(r: &GuestRequest, page: u64) -> Result<Value, Diagnostic> {
    let scope = format!("(Company = '{}' OR Company = '')", r.company.replace('\'', "''"));
    page_args(r, json!({"whereClauseLangTran":format!("{scope} AND (SystemFlag = false OR CustomTrans = true OR OrgTextID < 0)")}), page)
}
/// Accumulate one page of a paged group read. Ok(Some) requests the next page; Ok(None) leaves
/// the combined response in `v` for the phase, exactly as one unpaged response would have been.
fn accumulate(r: &mut GuestRequest, v: &mut Value) -> Result<Option<GuestReply>, Diagnostic> {
    let phase = r.state["phase"].as_str().unwrap_or("").to_owned();
    let resume = || err("Herbert could not continue this download from its saved progress (resource page). Start the download again.");
    if let Some((service, tables)) = group(&phase) {
        let more = v["parameters"]["morePages"].as_bool() == Some(true);
        if more && rows(&v["returnObj"], tables[0])?.is_empty() {
            return Err(err("An Epicor resource page said more pages follow but carried no rows."));
        }
        if !r.state["groupRows"].is_object() {
            r.state["groupRows"] = json!({});
        }
        for t in tables {
            let rs = rows(&v["returnObj"], t)?;
            let acc = &mut r.state["groupRows"][*t];
            if !acc.is_array() {
                *acc = json!([]);
            }
            acc.as_array_mut().unwrap().extend(rs.iter().cloned());
        }
        if more {
            let page = r.state["groupPage"].as_u64().unwrap_or(1) + 1;
            if !r.state["groupArgs"].is_object() {
                return Err(resume());
            }
            let args = page_args(r, r.state["groupArgs"].clone(), page)?;
            r.state["groupPage"] = json!(page);
            let mut state = r.state.take();
            state["phase"] = json!(phase);
            return Ok(Some(crate::native(service, "GetRows", args, false, state)));
        }
        *v = json!({"returnObj":r.state["groupRows"].take(),"parameters":{"morePages":false}});
        for k in ["groupRows", "groupArgs", "groupPage"] {
            r.state.as_object_mut().unwrap().remove(k);
        }
    } else if matches!(phase.as_str(), "files" | "links" | "image-bytes" | "owner" | "bytes" | "standalone-theme-metadata" | "standalone-theme-bytes") {
        // A phase reports its own message for a response without rows.
        let Some(rs) = v["rows"].as_array() else { return Ok(None) };
        let more = size(r)?.is_some_and(|size| rs.len() == size as usize);
        if !r.state["queryRows"].is_array() {
            r.state["queryRows"] = json!([]);
        }
        r.state["queryRows"].as_array_mut().unwrap().extend(rs.iter().cloned());
        if more {
            let after = rs.last().and_then(|row| row["SysRowID"].as_str())
                .ok_or_else(|| err("A full Epicor resource page carried no Epicor ID to continue from."))?;
            if r.state["queryAfter"].as_str() == Some(after) {
                return Err(err("The Epicor resource page did not advance."));
            }
            let spec: QuerySpec = serde_json::from_value(r.state["query"].clone()).map_err(|_| resume())?;
            r.state["queryAfter"] = json!(after);
            return Ok(Some(query_page(spec, r.state.take())));
        }
        *v = json!({"rows":r.state["queryRows"].take()});
        for k in ["query", "queryRows", "queryAfter"] {
            r.state.as_object_mut().unwrap().remove(k);
        }
    }
    Ok(None)
}
fn theme(r: &GuestRequest, body: &Value) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let roots = rows(body, "Theme")?;
    let children = rows(body, "ThemeResource")?;
    let mut result = vec![];
    let mut seen = BTreeSet::new();
    for root in roots {
        if root["SystemFlag"] != false || !seen.insert(root["ThemeID"].clone().to_string()) {
            return Err(err("Invalid or repeated custom theme."));
        }
        let resources: Vec<_> = children
            .iter()
            .filter(|c| c["ThemeID"] == root["ThemeID"])
            .cloned()
            .collect();
        let ds = json!({"Theme":[root],"ThemeResource":resources,"ExtensionTables":[]});
        let mut files = vec![
            json!({"name":"RestoreTheme.request.json","text":serde_json::to_string_pretty(&recovery(&ds)).unwrap()}),
        ];
        files.push(json!({"name":"Theme.xml","text":root["ThemeXML"].as_str().ok_or_else(||err("Epicor returned a theme without its content, so it was not saved."))?}));
        let mut binaries = vec![];
        for child in &resources {
            if child["SystemFlag"] != false {
                return Err(err("System theme resource was returned."));
            }
            let rid = safe_id(child)?;
            if let Some(b) = child["BinaryResource"].as_str() {
                binaries.push(binary(format!("Resources/{rid}.bin"), b)?);
            }
            if let Some(t) = child["Resource"].as_str() {
                files.push(json!({"name":format!("Resources/{rid}.txt"),"text":t}));
            }
        }
        result.push((
            id("Theme", root, &["ThemeID"])?,
            complete(ds, files, binaries),
        ));
    }
    if children
        .iter()
        .any(|c| !roots.iter().any(|p| p["ThemeID"] == c["ThemeID"]))
    {
        return Err(err("Theme resource has no parent."));
    }
    let _ = r;
    Ok(result)
}
pub fn handle(r: GuestRequest) -> GuestReply {
    let from=r.state.clone();
    if !matches!(r.operation.as_str(),"discover"|"materialize"){return handle_inner(r);}
    if let Err(diagnostic)=family(&from){return GuestReply::Failure {diagnostic};}
    match selected_reply(handle_inner(r),&from){Ok(reply)=>reply,Err(diagnostic)=>GuestReply::Failure {diagnostic}}
}
fn handle_inner(r: GuestRequest) -> GuestReply {
    let stream = r
        .cursor
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let discovery = r.operation == "discover";
    match drive(r) {
        Ok(v) => v,
        Err(d) if discovery => GuestReply::Inventory {
            identities: vec![],
            next_cursor: (stream < 4).then(|| (stream + 1).to_string()),
            authoritative: false,
            diagnostics: vec![d],
        },
        Err(d) => GuestReply::Failure { diagnostic: d },
    }
}
fn drive(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if r.state["phase"] == "output" {
        return Ok(output(r.state));
    }
    if r.operation != "discover" && r.operation != "materialize" {
        return Err(err(
            "Custom resources are downloaded as complete groups. Start the download again to choose resources.",
        ));
    }
    if r.company.is_empty() {
        return Err(err("A company is required for resource collection."));
    }
    if !r.state.is_object() {
        r.state = json!({});
    }
    let stream = r
        .cursor
        .as_deref()
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| err("Invalid resource stream saved position."))?
        .unwrap_or(0);
    if r.state["phase"].is_null()&&r.response.is_none()&&r.error.is_none() {
        let family=family(&r.state)?;
        if stream<=4&&family!="all"&&stream_family(stream)!=family {
            let next=((stream+1)..=4).find(|stream|stream_family(*stream)==family).map(|stream|stream.to_string());
            return Ok(GuestReply::Inventory {identities:vec![],next_cursor:next,authoritative:true,diagnostics:vec![]});
        }
    }
    if r.state["phase"].is_null() {
        r.state["prepare"] = r.state["_hostPreparePage"].clone();
        r.state["independent"] = r.state["_hostIndependentWorkItems"].clone();
        let scope = format!(
            "(Company = '{}' OR Company = '')",
            r.company.replace('\'', "''")
        );
        return Ok(match stream {
            0 => group_action(
                r,
                "themes",
                json!({"whereClauseTheme":"SystemFlag = false","whereClauseThemeResource":"SystemFlag = false"}),
            )?,
            1 => {
                let args = translation_page(&r, 1)?;
                r.state["translationPage"] = json!(1);
                r.state["graph"] = json!({"LangTran":[]});
                action(r, "translations", "Ice.BO.LangTranSvc", args)
            }
            2 => group_action(r, "images", json!({"whereClauseImage":scope}))?,
            3 => {
                let fields = [
                    "SysRowID",
                    "ForeignSysRowID",
                    "RelatedToSchemaName",
                    "RelatedToTable",
                    "Company",
                    "TenantID",
                    "SecCode",
                    "FileName",
                    "Category",
                    "FileType",
                    "CreatedBy",
                    "CreatedOn",
                    "ModifiedBy",
                    "ModifiedOn",
                    "SysRevID",
                ]
                .into_iter()
                .map(|f| QueryField {
                    table: "R".into(),
                    field: f.into(),
                    alias: f.into(),
                    expression: None,
                })
                .collect();
                let state = r.state.clone();
                query(
                    &r,
                    "files",
                    "Ice",
                    "FileStore",
                    fields,
                    vec![("Company", json!([r.company, "", "All"]))],
                    state,
                )?
            }
            4 => standalone_theme_metadata(r)?,
            _ => return Err(err("Herbert could not continue this download from its saved progress (resource group). Start the download again.")),
        });
    }
    let mut v = response(&r)?.clone();
    if let Some(next) = accumulate(&mut r, &mut v)? {
        return Ok(next);
    }
    match r.state["phase"].as_str().unwrap_or("") {
        "themes" => {
            return Ok(finish(
                theme(&r, &v["returnObj"])?,
                Some("1".into()),
                r.state["prepare"] == true,
                r.state["independent"] == true,
            ));
        }
        "translations" => {
            let more = v["parameters"]["morePages"].as_bool() == Some(true);
            let trs = rows(&v["returnObj"], "LangTran")?;
            if more && trs.is_empty() {
                return Err(err("The translation page did not advance."));
            }
            for t in trs {
                scoped(t, &r.company)?;
                if !(t["SystemFlag"] == false
                    || t["CustomTrans"] == true
                    || t["OrgTextID"].as_i64().is_some_and(|n| n < 0))
                {
                    return Err(err("Translation is not authored."));
                }
            }
            // Pages accumulate into the one translation library graph built below.
            if !r.state["graph"]["LangTran"].is_array() {
                r.state["graph"] = json!({"LangTran":[]});
            }
            r.state["graph"]["LangTran"].as_array_mut().unwrap().extend(trs.iter().cloned());
            if more {
                let page = r.state["translationPage"].as_u64().unwrap_or(1) + 1;
                let args = translation_page(&r, page)?;
                r.state["translationPage"] = json!(page);
                return Ok(action(r, "translations", "Ice.BO.LangTranSvc", args));
            }
            let scope = format!(
                "(Company = '{}' OR Company = '')",
                r.company.replace('\'', "''")
            );
            return group_action(
                r,
                "languages",
                json!({"whereClauseLangName":scope,"whereClauseCulture":"","whereClauseNomenclatureModule":""}),
            );
        }
        "languages" => {
            let names = rows(&v["returnObj"], "LangName")?;
            for row in names {
                scoped(row, &r.company)?;
            }
            r.state["allLangName"] = json!(names);
            r.state["graph"]["LangName"] = json!(
                names
                    .iter()
                    .filter(|r| r["SystemFlag"] == false)
                    .cloned()
                    .collect::<Vec<_>>()
            );
            let scope = format!(
                "(Company = '{}' OR Company = '') AND SystemFlag = false",
                r.company.replace('\'', "''")
            );
            return group_action(r, "originals", json!({"whereClauseLangOrg":scope}));
        }
        "originals" => {
            r.state["graph"]["CustomLangOrg"] = json!(rows(&v["returnObj"], "LangOrg")?);
            r.state["linkTable"] = json!(0);
            return links(r);
        }
        "links" => {
            let t = r.state["linkName"].as_str().unwrap().to_owned();
            r.state["graph"][t] = json!(
                v["rows"]
                    .as_array()
                    .ok_or_else(|| err("Linked translation read omitted rows."))?
            );
            r.state["linkTable"] = json!(r.state["linkTable"].as_u64().unwrap() + 1);
            return links(r);
        }
        "images" => {
            let images = rows(&v["returnObj"], "Image")?;
            for i in images {
                scoped(i, &r.company)?;
                safe_id(i)?;
            }
            r.state["graph"] = json!({"Image":images});
            let scope = format!(
                "(Company = '{}' OR Company = '')",
                r.company.replace('\'', "''")
            );
            return group_action(
                r,
                "layers",
                json!({"whereClauseImageLayerHeader":scope,"whereClauseImageLayerDetail":""}),
            );
        }
        "layers" => {
            for t in ["ImageLayerHeader", "ImageLayerDetail"] {
                let rs = rows(&v["returnObj"], t)?;
                for row in rs {
                    scoped(row, &r.company)?;
                }
                r.state["graph"][t] = json!(rs);
            }
            let scope = format!(
                "(Company = '{}' OR Company = '')",
                r.company.replace('\'', "''")
            );
            return group_action(
                r,
                "categories",
                json!({"whereClauseImageCategory":scope,"whereClauseImageSubCategory":""}),
            );
        }
        "categories" => {
            for t in ["ImageCategory", "ImageSubCategory"] {
                let rs = rows(&v["returnObj"], t)?;
                for row in rs {
                    scoped(row, &r.company)?;
                }
                r.state["graph"][t] = json!(rs);
            }
            let ids: BTreeSet<String> = r.state["graph"]["Image"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|r| [r["ImageSysRowID"].as_str(), r["ThumbnailSysRowID"].as_str()])
                .flatten()
                .filter(|s| !s.is_empty() && *s != "00000000-0000-0000-0000-000000000000")
                .map(str::to_ascii_lowercase)
                .collect();
            if !ids.is_empty() {
                r.state["imageFileIDs"] = json!(ids);
                return query(
                    &r,
                    "image-bytes",
                    "Ice",
                    "FileStore",
                    all(),
                    vec![
                        ("SysRowID", r.state["imageFileIDs"].clone()),
                        ("Company", json!([r.company, "", "All"])),
                    ],
                    r.state.clone(),
                );
            }
            return image_finish(r);
        }
        "image-bytes" => {
            let rs = v["rows"]
                .as_array()
                .ok_or_else(|| err("Image resource read omitted rows."))?;
            let ids: BTreeSet<String> = serde_json::from_value(r.state["imageFileIDs"].clone())
                .map_err(|_| err("Herbert could not continue this download from its saved progress (selected images). Start the download again."))?;
            let mut seen = BTreeSet::new();
            let mut files = vec![];
            let mut ordered = rs.iter().collect::<Vec<_>>();
            ordered.sort_by(|a, b| a["SysRowID"].as_str().cmp(&b["SysRowID"].as_str()));
            // Sort images once and index each image role by its lowercase file ID, in image then
            // role order: the same matches, in the same order, as scanning every image per file.
            let mut images = r.state["graph"]["Image"].as_array().cloned().unwrap_or_default();
            images.sort_by(|a, b| {
                a["ImageID"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["ImageID"].as_str().unwrap_or(""))
            });
            let roles = [
                ("ImageSysRowID", "ImageFileName", "Original"),
                ("ThumbnailSysRowID", "ImageFileThumbnail", "Thumbnails"),
            ];
            let index = image_index(&images, &roles.map(|(key, _, _)| key));
            let mut names = BTreeSet::new();
            for row in ordered {
                scoped(row, &r.company)?;
                let rid = safe_id(row)?;
                if !ids.contains(&rid.to_ascii_lowercase())
                    || !seen.insert(rid.to_ascii_lowercase())
                {
                    return Err(err("Image resource identity mismatch."));
                }
                let encoded = row["Content"]
                    .as_str()
                    .ok_or_else(|| err("Image original content missing."))?;
                for &(i, role) in index.get(&rid.to_ascii_lowercase()).into_iter().flatten() {
                    let image = &images[i];
                    let (_, filename, folder) = roles[role];
                    {
                        let original = image[filename]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .or_else(|| row["FileName"].as_str())
                            .unwrap_or("");
                        let name = resource_filename(
                            original,
                            row["FileType"].as_str().unwrap_or(""),
                            encoded,
                        )?;
                        let mut path = format!(
                            "{folder}/{}/{name}",
                            readable_component(image["ImageID"].as_str().unwrap_or("Unnamed"))
                        );
                        if names.contains(&path.to_lowercase()) {
                            path = format!(
                                "{folder}/{} - {}/{name}",
                                readable_component(image["ImageID"].as_str().unwrap_or("Unnamed")),
                                safe_id(image)?
                            );
                        }
                        names.insert(path.to_lowercase());
                        files.push(binary(path.clone(), encoded)?);
                        if !r.state["graph"]["BinaryFileIndex"].is_array() {
                            r.state["graph"]["BinaryFileIndex"] = json!([]);
                        }
                        r.state["graph"]["BinaryFileIndex"].as_array_mut().unwrap().push(json!({"path":path,"FileStoreSysRowID":rid,"ImageID":image["ImageID"],"role":folder,"originalFileName":original}));
                    }
                }
            }
            if seen != ids {
                return Err(err("Original image or thumbnail resource is inaccessible."));
            }
            r.state["graph"]["FileStore"] = json!(rs);
            r.state["imageBinaries"] = json!(files);
            return image_finish(r);
        }
        "files" => {
            let candidates = v["rows"]
                .as_array()
                .ok_or_else(|| err("FileStore inventory omitted rows."))?;
            let owners: Vec<[String; 2]> =
                serde_json::from_str(include_str!("filestore-owner-tables.json")).unwrap();
            let mut selected = vec![];
            let mut seen = BTreeSet::new();
            for row in candidates {
                scoped(row, &r.company)?;
                let rid = safe_id(row)?;
                if !seen.insert(rid.to_ascii_lowercase()) {
                    return Err(err("Epicor returned the same stored file more than once, so Herbert cannot tell which copy is correct. It was not saved."));
                }
                if owners.iter().any(|o| {
                    row["RelatedToSchemaName"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&o[0]))
                        && row["RelatedToTable"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&o[1]))
                }) {
                    selected.push(row.clone());
                }
            }
            r.state["files"] = json!(selected);
            r.state["ownerIndex"] = json!(0);
            r.state["owners"] = json!(
                owners
                    .into_iter()
                    .filter(|o| selected.iter().any(|f| f["RelatedToSchemaName"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(&o[0]))
                        && f["RelatedToTable"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&o[1]))))
                    .collect::<Vec<_>>()
            );
            r.state["ownerRows"] = json!({});
            return next_owner(r);
        }
        "owner-schema" => {
            let fs = rows(&v["returnObj"], "TableFieldList")?;
            if !fs.iter().any(|f| f["FieldName"] == "SysRowID") {
                return Err(err("Declared resource owner has no Epicor ID."));
            }
            r.state["ownerHasSystemFlag"] =
                json!(fs.iter().any(|f| f["FieldName"] == "SystemFlag"));
            let o = r.state["owners"][r.state["ownerIndex"].as_u64().unwrap() as usize].clone();
            let ids: Vec<_> = r.state["files"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| {
                    f["RelatedToSchemaName"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(o[0].as_str().unwrap()))
                        && f["RelatedToTable"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(o[1].as_str().unwrap()))
                })
                .map(|f| f["ForeignSysRowID"].clone())
                .collect();
            let fields = ["SysRowID", "Company", "SystemFlag"]
                .into_iter()
                .filter(|name| fs.iter().any(|f| f["FieldName"] == *name))
                .map(|name| QueryField {
                    table: "R".into(),
                    field: name.into(),
                    alias: name.into(),
                    expression: None,
                })
                .collect();
            return query(
                &r,
                "owner",
                o[0].as_str().unwrap(),
                o[1].as_str().unwrap(),
                fields,
                vec![("SysRowID", json!(ids))],
                r.state.clone(),
            );
        }
        "owner" => {
            let rs = v["rows"]
                .as_array()
                .ok_or_else(|| err("Owner read omitted rows."))?;
            let o = r.state["owners"][r.state["ownerIndex"].as_u64().unwrap() as usize].clone();
            let key = format!("{}.{}", o[0].as_str().unwrap(), o[1].as_str().unwrap())
                .to_ascii_lowercase();
            for row in rs {
                safe_id(row)?;
                if r.state["ownerHasSystemFlag"] == true && !row["SystemFlag"].is_boolean() {
                    return Err(err("Owner system classification is missing."));
                }
            }
            r.state["ownerRows"][key] = json!(rs);
            r.state["ownerIndex"] = json!(r.state["ownerIndex"].as_u64().unwrap() + 1);
            return next_owner(r);
        }
        "bytes" => {
            let rs = v["rows"]
                .as_array()
                .ok_or_else(|| err("Bulk resource read omitted rows."))?;
            let expected = r.state["selected"].as_array().unwrap();
            let mut records = vec![];
            let mut seen = BTreeSet::new();
            for row in rs {
                let rid = safe_id(row)?;
                if !seen.insert(rid.to_ascii_lowercase()) {
                    return Err(err("Bulk resource read repeats a file."));
                }
                let old = expected
                    .iter()
                    .find(|r| {
                        r["SysRowID"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(rid))
                    })
                    .ok_or_else(|| err("Bulk resource read returned an unselected file."))?;
                for (field, value) in old.as_object().unwrap() {
                    if row.get(field) != Some(value) {
                        return Err(err("Resource metadata changed during collection."));
                    }
                }
                let encoded = row["Content"]
                    .as_str()
                    .ok_or_else(|| err("Original FileStore content is missing."))?;
                let name = resource_filename(
                    row["FileName"].as_str().unwrap_or(""),
                    row["FileType"].as_str().unwrap_or(""),
                    encoded,
                )?;
                let b = binary(format!("Original/{name}"), encoded)?;
                let key = format!(
                    "{}.{}",
                    row["RelatedToSchemaName"].as_str().unwrap(),
                    row["RelatedToTable"].as_str().unwrap()
                )
                .to_ascii_lowercase();
                let owner = r.state["ownerRows"][key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| {
                        o["SysRowID"].as_str().is_some_and(|s| {
                            row["ForeignSysRowID"]
                                .as_str()
                                .is_some_and(|id| s.eq_ignore_ascii_case(id))
                        })
                    })
                    .cloned();
                let mut metadata = row.clone();
                metadata.as_object_mut().unwrap().remove("Content");
                let data = json!({"FileStore":[metadata],"Owner":owner.into_iter().collect::<Vec<_>>(),"ContentEncoding":"base64","OriginalContent":encoded});
                // SysRowID is FileStore's native primary key. Owner links remain
                // exact definition fields, rather than inflating every pathname.
                records.push((
                    id("FileStoreCustomizationResource", row, &["SysRowID"])?,
                    complete(data, vec![], vec![b]),
                ));
            }
            if seen.len() != expected.len() {
                return Err(err("Bulk resource read is incomplete."));
            }
            return Ok(finish(
                records,
                Some("4".into()),
                r.state["prepare"] == true,
                r.state["independent"] == true,
            ));
        }
        "standalone-theme-metadata" => {
            let rs = v["rows"]
                .as_array()
                .ok_or_else(|| err("Theme resource metadata read omitted rows."))?;
            let mut selected = vec![];
            let mut seen = BTreeSet::new();
            for row in rs {
                let rid = safe_id(row)?;
                if !seen.insert(rid.to_ascii_lowercase()) {
                    return Err(err("Theme resource metadata repeats a row."));
                }
                if row["OwnerSystemFlag"] == false {
                    continue;
                }
                selected.push(row.clone());
            }
            if selected.is_empty() {
                return Ok(finish(
                    vec![],
                    None,
                    r.state["prepare"] == true,
                    r.state["independent"] == true,
                ));
            }
            let ids: Vec<_> = selected.iter().map(|r| r["SysRowID"].clone()).collect();
            r.state["themeResources"] = json!(selected);
            return query(
                &r,
                "standalone-theme-bytes",
                "Ice",
                "ThemeResource",
                all(),
                vec![("SysRowID", json!(ids)), ("SystemFlag", json!(false))],
                r.state.clone(),
            );
        }
        "standalone-theme-bytes" => {
            let rs = v["rows"]
                .as_array()
                .ok_or_else(|| err("Standalone theme resources omitted rows."))?;
            let selected = r.state["themeResources"].as_array().unwrap();
            let mut records = vec![];
            let mut seen = BTreeSet::new();
            for row in rs {
                let rid = safe_id(row)?;
                if !seen.insert(rid.to_ascii_lowercase()) {
                    return Err(err("Theme resource body repeats a row."));
                }
                let meta = selected
                    .iter()
                    .find(|m| m["SysRowID"] == row["SysRowID"])
                    .ok_or_else(|| err("Unexpected theme resource body."))?;
                for f in [
                    "ThemeID",
                    "ResourceType",
                    "ResourceName",
                    "SysRowID",
                    "SysRevID",
                    "SystemFlag",
                ] {
                    if meta[f] != row[f] {
                        return Err(err("Theme resource changed during collection."));
                    }
                }
                let owner = if meta["OwnerThemeID"].is_null() {
                    json!([])
                } else {
                    json!([{"ThemeID":meta["OwnerThemeID"],"SystemFlag":meta["OwnerSystemFlag"],"ThemeDesc":meta["OwnerThemeDesc"]}])
                };
                let ds = json!({"Theme":[],"ThemeResource":[row],"ExtensionTables":[]});
                let files = vec![
                    json!({"name":"Resource.txt","text":row["Resource"].as_str().ok_or_else(||err("Theme resource text missing."))?}),
                    json!({"name":"RestoreThemeResource.request.json","text":serde_json::to_string_pretty(&recovery(&ds)).unwrap()}),
                ];
                let bytes = binary(
                    "Resource.bin".into(),
                    row["BinaryResource"]
                        .as_str()
                        .ok_or_else(|| err("Theme resource original bytes missing."))?,
                )?;
                records.push((id("StandaloneThemeResource",row,&["SysRowID"] )?,complete(json!({"ThemeResource":[row],"OwnerTheme":owner,"requiresExistingTheme":row["ThemeID"]}),files,vec![bytes])));
            }
            if seen.len() != selected.len() {
                return Err(err("Standalone theme resource read is incomplete."));
            }
            return Ok(finish(
                records,
                None,
                r.state["prepare"] == true,
                r.state["independent"] == true,
            ));
        }
        _ => return Err(err("Unknown custom-resource resume point.")),
    }
}
fn image_finish(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let graph = r.state["graph"].take();
    let any = graph
        .as_object()
        .unwrap()
        .values()
        .any(|v| v.as_array().is_some_and(|a| !a.is_empty()));
    let binaries = serde_json::from_value(r.state["imageBinaries"].take()).unwrap_or_default();
    let records = if any {
        let root = json!({"Company":r.company,"Library":"Images"});
        vec![(
            id("ImageLibrary", &root, &["Library"])?,
            complete(graph, vec![], binaries),
        )]
    } else {
        vec![]
    };
    Ok(finish(
        records,
        Some("3".into()),
        r.state["prepare"] == true,
        r.state["independent"] == true,
    ))
}
fn links(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    // Complete selected relationships, including linked system dictionaries.
    let stage = r.state["linkTable"].as_u64().unwrap() as usize;
    let graph = &r.state["graph"];
    let ts = graph["LangTran"].as_array().unwrap();
    let (table, field, ids) = match stage {
        0 => (
            "LangOrg",
            "OrgTextID",
            ts.iter()
                .flat_map(|r| [r["OrgTextID"].clone(), r["OrigOrgTextID"].clone()])
                .chain(
                    graph["CustomLangOrg"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| r["OrgTextID"].clone()),
                )
                .filter(|v| v.as_i64().is_some_and(|n| n != 0))
                .collect::<Vec<_>>(),
        ),
        1 => (
            "LangXref",
            "OrgTextID",
            ts.iter().map(|r| r["OrgTextID"].clone()).collect(),
        ),
        2 => (
            "LangProg",
            "ProgramID",
            ts.iter()
                .chain(graph["LangXref"].as_array().into_iter().flatten())
                .map(|r| r["ProgramID"].clone())
                .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                .collect(),
        ),
        3 => {
            let names = r.state["allLangName"].as_array().unwrap();
            let mut needed: BTreeSet<String> = ts
                .iter()
                .map(|r| r["LangNameID"].as_str().unwrap_or("").to_ascii_lowercase())
                .chain(
                    graph["LangName"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|r| r["ParentLangID"].as_str().map(str::to_ascii_lowercase)),
                )
                .filter(|s| !s.is_empty())
                .collect();
            let mut known = BTreeSet::new();
            let mut references = vec![];
            let mut unresolved = vec![];
            while let Some(key) = needed.pop_first() {
                if !known.insert(key.clone()) {
                    continue;
                }
                let found: Vec<_> = names
                    .iter()
                    .filter(|r| {
                        r["LangNameID"]
                            .as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&key))
                    })
                    .collect();
                if found.is_empty() {
                    unresolved.push(json!({"LangNameID":key,"reason":"not-present-in-native-company-or-global-catalog"}));
                }
                for row in found {
                    references.push(row.clone());
                    if let Some(parent) = row["ParentLangID"].as_str().filter(|s| !s.is_empty()) {
                        needed.insert(parent.to_ascii_lowercase());
                    }
                }
            }
            r.state["graph"]["UnresolvedLanguageReferences"] = json!(unresolved);
            r.state["graph"]["ReferencedLangName"] = json!(references);
            r.state["linkTable"] = json!(4);
            return links(r);
        }
        _ => {
            let mut graph = r.state["graph"].take();
            let any = graph
                .as_object()
                .unwrap()
                .values()
                .any(|v| v.as_array().is_some_and(|r| !r.is_empty()));
            let mut files = vec![];
            if any {
                let mut translation = vec![];
                let mut unavailable = vec![];
                let source_translations = graph["LangTran"].as_array().unwrap();
                // Index once instead of scanning every translation, link and original per row.
                let siblings = sibling_index(source_translations);
                let xrefs = graph["LangXref"].as_array().map(|rows| first_by_text(rows, true));
                let originals = graph["LangOrg"].as_array().map(|rows| first_by_text(rows, false));
                for row in source_translations {
                    let (general, programs) = siblings[&sibling_key(row)];
                    let general = general.map(|i| &source_translations[i]);
                    // TranText carries general and program text in two fields.
                    // A separate blank-program row cannot create custom text.
                    if row["ProgramID"] == "" && programs {
                        continue;
                    }
                    let text = json!([row["Company"], row["OrgTextID"]]).to_string();
                    let program = if row["ProgramID"] != "" {
                        Some(row["ProgramID"].clone())
                    } else {
                        xrefs.as_ref().unwrap().get(&text).map(|v| v["ProgramID"].clone())
                    };
                    let original = originals.as_ref().unwrap().get(&text).copied();
                    let (Some(program), Some(original)) = (program, original) else {
                        unavailable.push(json!({"SysRowID":row["SysRowID"],"reason":"native-import-requires-original-text-and-program-relationship","sourcePreserved":true}));
                        continue;
                    };
                    // The stored GUID is nullable, but native TranText's input
                    // setter throws on JSON null in both tested versions.
                    // Use its empty-GUID default in the import projection only.
                    let nomenclature = row["NomenclatureID"]
                        .as_str()
                        .unwrap_or("00000000-0000-0000-0000-000000000000");
                    translation.push(json!({"Company":row["Company"],"LanguageID":row["LangNameID"],"OrgTextID":if row["OrgTextID"].as_i64().is_some_and(|n|n<0){json!(0)}else{row["OrgTextID"].clone()},"OrigOrgTextID":row["OrigOrgTextID"],"ProgramID":program,"OriginalText":original["OrgText"],"TranslatedText":general.map(|v|v["TransText"].clone()).unwrap_or(json!("")),"ProgTranText":if row["ProgramID"]==""{json!("")}else{row["TransText"].clone()},"NomenclatureID":nomenclature,"SystemFlag":row["SystemFlag"],"ErrorCode":0,"RowMod":"A"}));
                }
                graph["UnavailableNativeTranslationImports"] = json!(unavailable);
                let request = json!({"ds":{"TranText":translation,"TranTextLayer":[],"ExtensionTables":[]},"bCustomization":true});
                files.push(json!({"name":"RestoreTranslations.request.json","text":serde_json::to_string_pretty(&request).unwrap()}));
                let request = recovery(
                    &json!({"LangName":graph["LangName"],"Culture":[],"NomenclatureModule":[],"ExtensionTables":[]}),
                );
                files.push(json!({"name":"RestoreLanguages.request.json","text":serde_json::to_string_pretty(&request).unwrap()}));
            }
            let records = if any {
                vec![(
                    id(
                        "TranslationLibrary",
                        &json!({"Company":r.company,"Library":"Translations"}),
                        &["Library"],
                    )?,
                    complete(graph, files, vec![]),
                )]
            } else {
                vec![]
            };
            return Ok(finish(
                records,
                Some("2".into()),
                r.state["prepare"] == true,
                r.state["independent"] == true,
            ));
        }
    };
    if ids.is_empty() {
        r.state["graph"][if stage == 3 {
            "ReferencedLangName"
        } else {
            table
        }] = json!([]);
        r.state["linkTable"] = json!(stage + 1);
        return links(r);
    }
    r.state["linkName"] = json!(if stage == 3 {
        "ReferencedLangName"
    } else {
        table
    });
    query(
        &r,
        "links",
        "Ice",
        table,
        all(),
        vec![(field, json!(ids)), ("Company", json!([r.company, ""]))],
        r.state.clone(),
    )
}
fn next_owner(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let i = r.state["ownerIndex"].as_u64().unwrap() as usize;
    if let Some(o) = r.state["owners"].get(i) {
        let args = json!({"schema":o[0],"dataTableID":o[1]});
        r.state["phase"] = json!("owner-schema");
        return Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.BO.BAQDesignerSvc".into(),
                method: "GetFieldList".into(),
                arguments: args,
                export: false,
            },
            state: r.state,
        });
    }
    let mut selected = vec![];
    for f in r.state["files"].as_array().unwrap() {
        let key = format!(
            "{}.{}",
            f["RelatedToSchemaName"].as_str().unwrap(),
            f["RelatedToTable"].as_str().unwrap()
        )
        .to_ascii_lowercase();
        let os = r.state["ownerRows"][key].as_array().unwrap();
        let matches: Vec<_> = os
            .iter()
            .filter(|o| {
                o["SysRowID"].as_str().is_some_and(|s| {
                    f["ForeignSysRowID"]
                        .as_str()
                        .is_some_and(|id| s.eq_ignore_ascii_case(id))
                })
            })
            .collect();
        if matches.len() > 1 {
            return Err(err("Resource owner identity repeats."));
        }
        if matches.iter().any(|o| {
            o["SystemFlag"] == true
                || o.get("Company")
                    .is_some_and(|c| c != "" && c != &json!(r.company))
        }) {
            continue;
        }
        // Orphans retain their explicit declared owner metadata for recovery.
        selected.push(f.clone());
    }
    if selected.is_empty() {
        return Ok(finish(
            vec![],
            Some("4".into()),
            r.state["prepare"] == true,
            r.state["independent"] == true,
        ));
    }
    let ids: Vec<_> = selected.iter().map(|r| r["SysRowID"].clone()).collect();
    r.state["selected"] = json!(selected);
    query(
        &r,
        "bytes",
        "Ice",
        "FileStore",
        all(),
        vec![
            ("SysRowID", json!(ids)),
            ("Company", json!([r.company, "", "All"])),
        ],
        r.state.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readable_files_keep_native_names_and_correct_image_extensions() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            resource_filename("assets/original/Company Logo.bin", "bin", &png).unwrap(),
            "Company Logo.png"
        );
        assert_eq!(
            resource_filename("C:\\images\\Ω.png", "png", &png).unwrap(),
            "Ω.png"
        );
        assert_eq!(resource_filename("", "png", &png).unwrap(), "Unnamed.png");
        assert_eq!(
            resource_filename("scripts/setup.js", "js", "").unwrap(),
            "setup.js"
        );
        assert_eq!(readable_component("../CON. "), "..%2FCON.%20");
        assert_eq!(readable_component("NUL.png"), "%4EUL.png");
        assert_ne!(readable_component("a/b"), readable_component("a%2Fb"));
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "custom-resources".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn permission_failure_preserves_independent_streams() {
        let mut r = request();
        r.state = json!({"phase":"themes"});
        r.error = Some(Diagnostic::new("DENIED", "native refusal"));
        let GuestReply::Inventory {
            next_cursor,
            authoritative,
            diagnostics,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(next_cursor.as_deref(), Some("1"));
        assert!(!authoritative);
        assert_eq!(diagnostics[0].code, "DENIED");
    }
    #[test]
    fn native_streams_and_metadata_only_filestore_are_paged_at_items_per_page() {
        let GuestReply::Action {
            action: HostAction::NativeRead { arguments, .. },
            ..
        } = handle(request())
        else {
            panic!()
        };
        assert_eq!((arguments["pageSize"].clone(), arguments["absolutePage"].clone()), (json!(100), json!(1)));
        let mut r = request();
        r.cursor = Some("3".into());
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!((query.page_size, query.page, query.order[0].field.as_str(), query.order[0].descending), (Some(100), 1, "SysRowID", false));
        assert!(
            query
                .fields
                .iter()
                .all(|f| f.field != "Content" && f.field != "*")
        );
        assert_eq!(query.parameters["p0"], json!(["C", "", "All"]));
    }
    #[test]
    fn empty_and_unicode_binary_bytes_are_exact_and_paths_use_row_identity() {
        for bytes in [b"".as_slice(), "// Ω\r\n😀".as_bytes()] {
            let s = base64::engine::general_purpose::STANDARD.encode(bytes);
            let f = binary("Original/row.bin".into(), &s).unwrap();
            assert_eq!(f["bytes"], bytes.len());
            assert_eq!(f["sha256"], format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(f["base64"], s);
        }
        assert!(binary("x".into(), "not base64!").is_err());
    }
    #[test]
    fn malformed_theme_children_cannot_be_reported_as_complete() {
        let root = json!({"ThemeID":"T","SystemFlag":false,"ThemeXML":"<Theme/>"});
        let body = json!({"Theme":[root],"ThemeResource":[{"ThemeID":"Other","SystemFlag":false}]});
        assert!(theme(&request(), &body).is_err());
    }
    #[test]
    fn incomplete_image_originals_fail_instead_of_becoming_metadata_only_success() {
        let mut r = request();
        r.cursor = Some("2".into());
        r.state =
            json!({"phase":"image-bytes","imageFileIDs":["11111111-1111-1111-1111-111111111111"]});
        r.response = Some(json!({"rows":[]}));
        let GuestReply::Inventory {
            diagnostics,
            authoritative,
            next_cursor,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert!(!authoritative);
        assert_eq!(next_cursor.as_deref(), Some("3"));
        assert!(diagnostics[0].message.contains("inaccessible"));
    }
    #[test]
    fn translation_projection_keeps_distinct_general_and_program_text_and_source_ids() {
        let mut r = request();
        let general = json!({"Company":"C","LangNameID":"L","OrgTextID":-1,"OrigOrgTextID":0,"ProgramID":"","TransText":"general Ω","NomenclatureID":null,"SystemFlag":false,"SysRowID":"general"});
        let mut program = general.clone();
        program["ProgramID"] = json!("P");
        program["TransText"] = json!("program é");
        r.state = json!({"linkTable":4,"graph":{"LangTran":[general.clone(),program],"LangOrg":[{"Company":"C","OrgTextID":-1,"OrgText":"original"}],"LangXref":[{"Company":"C","OrgTextID":-1,"ProgramID":"P"}],"LangName":[]}});
        let GuestReply::ReadyRecords { records, .. } = links(r.clone()).unwrap() else {
            panic!()
        };
        let data = &records[0].1;
        let req: Value =
            serde_json::from_str(data["herbertSources"]["files"][0]["text"].as_str().unwrap())
                .unwrap();
        let rs = req["ds"]["TranText"].as_array().unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0]["TranslatedText"], "general Ω");
        assert_eq!(rs[0]["ProgTranText"], "program é");
        assert_eq!(rs[0]["OrgTextID"], 0);
        assert_eq!(
            rs[0]["NomenclatureID"],
            "00000000-0000-0000-0000-000000000000"
        );
        assert_eq!(data["LangTran"][0], general);
        r.state["graph"]["LangTran"] = json!([general]);
        r.state["graph"]["LangXref"] = json!([]);
        let GuestReply::ReadyRecords { records, .. } = links(r).unwrap() else {
            panic!()
        };
        assert_eq!(
            records[0].1["UnavailableNativeTranslationImports"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(records[0].1["LangTran"][0]["OrgTextID"], -1);
    }

    #[test]
    fn translations_are_read_in_pages_of_items_per_page() {
        let mut r=request();r.cursor=Some("1".into());r.page_size=Some(2);
        let GuestReply::Action {action:HostAction::NativeRead {arguments,..},state}=handle(r.clone())else{panic!()};
        assert_eq!((arguments["pageSize"].clone(),arguments["absolutePage"].clone()),(json!(2),json!(1)));
        let row=|id:i64|json!({"Company":"C","LangNameID":"de","OrgTextID":-id,"SystemFlag":false,"CustomTrans":true});
        r.state=state;r.response=Some(json!({"returnObj":{"LangTran":[row(1),row(2)]},"parameters":{"morePages":true}}));
        let GuestReply::Action {action:HostAction::NativeRead {service,arguments,..},state}=handle(r.clone())else{panic!()};
        assert_eq!(service,"Ice.BO.LangTranSvc");assert_eq!((arguments["pageSize"].clone(),arguments["absolutePage"].clone()),(json!(2),json!(2)));
        r.state=state;r.response=Some(json!({"returnObj":{"LangTran":[row(3)]},"parameters":{"morePages":false}}));
        let GuestReply::Action {action:HostAction::NativeRead {service,..},state}=handle(r.clone())else{panic!()};
        assert_eq!(service,"Ice.BO.LangNameSvc");assert_eq!(state["graph"]["LangTran"].as_array().unwrap().len(),3);
        // A page that claims more but carries nothing, or omits the flag, is refused.
        let mut stuck=r.clone();stuck.state=json!({"phase":"translations","translationPage":1});stuck.response=Some(json!({"returnObj":{"LangTran":[]},"parameters":{"morePages":true}}));
        assert!(matches!(handle(stuck.clone()),GuestReply::Inventory {authoritative:false,..}));
        stuck.response=Some(json!({"returnObj":{"LangTran":[row(1)]}}));assert!(response(&stuck).is_err());
        // A cleared Fallback page size still pages, at the host's documented default.
        let mut unsized_request=request();unsized_request.cursor=Some("1".into());unsized_request.page_size=None;
        let GuestReply::Action {action:HostAction::NativeRead {arguments,..},..}=handle(unsized_request.clone())else{panic!()};
        assert_eq!((arguments["pageSize"].clone(),arguments["absolutePage"].clone()),(json!(OperatingSettings::default().page_size.unwrap()),json!(1)));
        unsized_request.page_size=Some(0);assert!(matches!(handle(unsized_request),GuestReply::Inventory {authoritative:false,..}));
    }
    #[test]
    fn resource_reads_use_the_request_page_size_and_custom_resources_declares_no_page_setting() {
        assert!(crate::response_policy::settings("custom-resources",false).iter().all(|setting|!matches!(setting,PluginSetting::Integer{..})));
        // Every read is paged: the request's size, else the host's documented default.
        assert_eq!(OperatingSettings::default().page_size,Some(500));
        let mut r=request();r.page_size=None;
        assert_eq!(size(&r).unwrap(),Some(500));
        r.page_size=Some(7);assert_eq!(size(&r).unwrap(),Some(7));
        r.page_size=Some(0);assert!(size(&r).is_err());
    }
    #[test]
    fn wave1_native_resources_require_explicit_completion_but_queries_do_not() {
        // Native group pages must state morePages as a boolean; true now continues paging.
        for phase in ["themes","languages","originals","images","layers","categories"] {
            for flag in [Value::Null,json!("false"),json!(0)] {
                let mut r=request();r.state=json!({"phase":phase});r.response=Some(json!({"parameters":{"morePages":flag}}));assert!(response(&r).is_err());
            }
            let mut r=request();r.state=json!({"phase":phase});r.response=Some(json!({}));assert!(response(&r).is_err());
            for flag in [true,false] {r.response=Some(json!({"parameters":{"morePages":flag}}));assert!(response(&r).is_ok());}
        }
        // Query pages carry no flag: a full keyset page means another may follow.
        for phase in ["files","links","image-bytes","owner","bytes","standalone-theme-metadata","standalone-theme-bytes"] {
            let mut r=request();r.state=json!({"phase":phase});r.response=Some(json!({"rows":[]}));assert!(response(&r).is_ok());
        }
        let mut r=request();r.state=json!({"phase":"themes"});r.response=Some(json!({"returnObj":{"Theme":[],"ThemeResource":[]}}));
        assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,next_cursor:Some(next),..} if next=="1"));
    }
    #[test]
    fn every_native_group_read_is_paged_and_accumulated_unchanged() {
        for phase in ["themes","languages","originals","images","layers","categories"] {
            let (service,tables)=group(phase).unwrap();
            let mut r=request();r.page_size=Some(2);
            r.state=json!({"phase":phase,"groupArgs":{"whereClauseX":"w"},"groupPage":1,"groupRows":{}});
            let page=|n:i64|json!({"returnObj":tables.iter().map(|t|(t.to_string(),json!([{"n":n}]))).collect::<serde_json::Map<_,_>>(),"parameters":{"morePages":n==1}});
            let mut v=page(1);
            let Some(GuestReply::Action{action:HostAction::NativeRead{service:s,arguments,..},state})=accumulate(&mut r,&mut v).unwrap() else{panic!("{phase}")};
            assert_eq!(s,service);assert_eq!((arguments["pageSize"].clone(),arguments["absolutePage"].clone(),arguments["whereClauseX"].clone()),(json!(2),json!(2),json!("w")));
            assert_eq!(state["phase"],phase);
            r.state=state;let mut v=page(2);
            assert!(accumulate(&mut r,&mut v).unwrap().is_none());
            for t in tables {assert_eq!(v["returnObj"][*t],json!([{"n":1},{"n":2}]));}
            assert_eq!(v["parameters"]["morePages"],false);
            assert!(r.state.get("groupRows").is_none()&&r.state.get("groupArgs").is_none());
            // A page that claims more but carries no rows is refused.
            let mut r=request();r.state=json!({"phase":phase,"groupArgs":{},"groupPage":1});
            let mut v=json!({"returnObj":tables.iter().map(|t|(t.to_string(),json!([]))).collect::<serde_json::Map<_,_>>(),"parameters":{"morePages":true}});
            assert!(accumulate(&mut r,&mut v).is_err());
        }
        // Every group starts at page 1 of the request's page size, or the host default without one.
        for stream in ["0","2"] {
            let mut r=request();r.cursor=Some(stream.into());r.page_size=Some(7);
            let GuestReply::Action{action:HostAction::NativeRead{arguments,..},..}=handle(r.clone())else{panic!()};
            assert_eq!((arguments["pageSize"].clone(),arguments["absolutePage"].clone()),(json!(7),json!(1)));
            r.page_size=None;let GuestReply::Action{action:HostAction::NativeRead{arguments,..},..}=handle(r)else{panic!()};
            assert_eq!(arguments["pageSize"],json!(500));
        }
    }
    #[test]
    fn themes_continue_across_pages_and_build_the_same_records() {
        let mut r=request();r.page_size=Some(1);
        let GuestReply::Action{state,..}=handle(r.clone())else{panic!()};
        let child=json!({"ThemeID":"A","SystemFlag":false,"SysRowID":"11111111-1111-1111-1111-111111111111","Resource":"r"});
        r.state=state;r.response=Some(json!({"returnObj":{"Theme":[{"ThemeID":"A","SystemFlag":false,"ThemeXML":"<A/>"}],"ThemeResource":[child]},"parameters":{"morePages":true}}));
        let GuestReply::Action{action:HostAction::NativeRead{service,arguments,..},state}=handle(r.clone())else{panic!()};
        assert_eq!(service,"Ice.BO.ThemeSvc");assert_eq!((arguments["absolutePage"].clone(),arguments["whereClauseTheme"].clone()),(json!(2),json!("SystemFlag = false")));
        r.state=state;r.response=Some(json!({"returnObj":{"Theme":[{"ThemeID":"B","SystemFlag":false,"ThemeXML":"<B/>"}],"ThemeResource":[]},"parameters":{"morePages":false}}));
        let GuestReply::ReadyRecords{records,state}=handle(r)else{panic!()};
        assert_eq!(records[0].0.keys["ThemeID"],"A");assert_eq!(records[0].1["ThemeResource"].as_array().unwrap().len(),1);
        assert_eq!(state["records"].as_array().unwrap().len(),2);assert_eq!(state["next"],"1");
    }
    #[test]
    fn every_resource_query_is_keyset_paged_and_accumulated_unchanged() {
        let mut r=request();r.cursor=Some("3".into());r.page_size=Some(2);
        let GuestReply::Action{action:HostAction::SharedQuery{query},state}=handle(r.clone())else{panic!()};
        assert_eq!((query.page_size,query.order[0].field.as_str()),(Some(2),"SysRowID"));
        assert!(query.criteria.iter().all(|c|c.operator!=">"));
        let id=|n:u8|format!("{n}{n}{n}{n}{n}{n}{n}{n}-1111-1111-1111-111111111111");
        r.state=state;let mut v=json!({"rows":[{"SysRowID":id(1)},{"SysRowID":id(2)}]});
        let Some(GuestReply::Action{action:HostAction::SharedQuery{query},state})=accumulate(&mut r,&mut v).unwrap()else{panic!()};
        assert_eq!((query.page_size,query.parameters["after"].clone()),(Some(2),json!(id(2))));
        assert!(query.criteria.iter().any(|c|c.table=="R"&&c.field=="SysRowID"&&c.operator==">"&&c.parameter=="after"));
        assert_eq!(query.parameters["p0"],json!(["C","","All"]));
        // A repeated keyset position cannot loop, and a full page needs an ID to continue from.
        let mut stuck=r.clone();stuck.state=state.clone();let mut v=json!({"rows":[{"SysRowID":id(3)},{"SysRowID":id(2)}]});
        assert!(accumulate(&mut stuck,&mut v).is_err());
        let mut v=json!({"rows":[{"SysRowID":id(3)},{"Other":1}]});stuck.state=state.clone();assert!(accumulate(&mut stuck,&mut v).is_err());
        r.state=state;let mut v=json!({"rows":[{"SysRowID":id(3)}]});
        assert!(accumulate(&mut r,&mut v).unwrap().is_none());
        assert_eq!(v,json!({"rows":[{"SysRowID":id(1)},{"SysRowID":id(2)},{"SysRowID":id(3)}]}));
        assert!(r.state.get("query").is_none()&&r.state.get("queryRows").is_none());
        // Every query phase pages, including image bytes, file bytes and theme metadata.
        let mut r=request();r.page_size=Some(5);r.state=json!({"graph":{"Image":[{"ImageSysRowID":id(1)}],"ImageCategory":[],"ImageSubCategory":[]},"phase":"categories"});
        r.response=Some(json!({"returnObj":{"ImageCategory":[],"ImageSubCategory":[]},"parameters":{"morePages":false}}));
        let GuestReply::Action{action:HostAction::SharedQuery{query},state}=handle(r)else{panic!()};
        assert_eq!((query.page_size,state["phase"].clone()),(Some(5),json!("image-bytes")));
        let mut r=request();r.cursor=Some("4".into());
        let GuestReply::Action{action:HostAction::SharedQuery{query},..}=handle(r.clone())else{panic!()};
        assert_eq!((query.page_size,query.order[0].field.as_str()),(Some(100),"SysRowID"));
        r.state=json!({"phase":"owner","ownerIndex":0,"owners":[["Ice","XXXDef"]],"ownerRows":{},"files":[{"SysRowID":id(1),"ForeignSysRowID":id(2),"RelatedToSchemaName":"Ice","RelatedToTable":"XXXDef"}]});
        r.page_size=Some(3);r.response=Some(json!({"rows":[]}));
        let GuestReply::Action{action:HostAction::SharedQuery{query},state}=handle(r)else{panic!()};
        assert_eq!((query.page_size,state["phase"].clone()),(Some(3),json!("bytes")));
    }
    #[test]
    fn image_and_translation_indexes_match_the_full_scans_with_duplicates() {
        let images=vec![json!({"ImageSysRowID":"AB","ThumbnailSysRowID":"ab"}),json!({"ImageSysRowID":"cd","ThumbnailSysRowID":"AB"}),json!({"ImageSysRowID":"ab","ThumbnailSysRowID":null}),json!({})];
        let keys=["ImageSysRowID","ThumbnailSysRowID"];let index=image_index(&images,&keys);
        for rid in ["ab","AB","Ab","cd","CD","ef"] {
            let mut scan=vec![];for (i,image) in images.iter().enumerate(){for (role,key) in keys.iter().enumerate(){if image[*key].as_str().is_some_and(|id|id.eq_ignore_ascii_case(rid)){scan.push((i,role));}}}
            assert_eq!(index.get(&rid.to_ascii_lowercase()).cloned().unwrap_or_default(),scan,"{rid}");
        }
        let t=|c:&str,l:&str,o:Value,p:Value,n:u8|json!({"Company":c,"LangNameID":l,"OrgTextID":o,"ProgramID":p,"n":n});
        let rows=vec![t("C","L",json!(1),json!(""),0),t("C","L",json!(1),json!("P"),1),t("C","L",json!(1),json!(""),2),t("C","L",json!(2),json!(""),3),t("C","M",json!(1),json!("Q"),4),t("","L",json!(1),json!(""),5),t("C","L",json!("1"),json!(""),6),t("C","L",json!(1.0),Value::Null,7),t("C","L",json!(2),json!("P"),8)];
        let siblings=sibling_index(&rows);
        for row in &rows {
            let scan:Vec<_>=rows.iter().filter(|v|v["Company"]==row["Company"]&&v["LangNameID"]==row["LangNameID"]&&v["OrgTextID"]==row["OrgTextID"]).collect();
            let (general,programs)=siblings[&sibling_key(row)];
            assert_eq!(general.map(|i|&rows[i]),scan.iter().find(|v|v["ProgramID"]=="").copied(),"{row}");
            assert_eq!(programs,scan.iter().any(|v|v["ProgramID"]!=""),"{row}");
            let text=json!([row["Company"],row["OrgTextID"]]).to_string();
            for program in [true,false] {
                let found=rows.iter().find(|v|v["Company"]==row["Company"]&&v["OrgTextID"]==row["OrgTextID"]&&(!program||v["ProgramID"]!=""));
                assert_eq!(first_by_text(&rows,program).get(&text).copied(),found,"{row}/{program}");
            }
        }
    }

}

#[cfg(test)]
mod selection_tests {
    use super::*;
    fn request(state:Value)->GuestRequest {GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"custom-resources".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state,response:None,error:None}}
    fn owner(kind:&str)->(Identity,Value){(Identity{category:"custom-resources".into(),kind:kind.into(),scope:Scope::Global,keys:BTreeMap::from([("ID".into(),kind.into())])},json!({"original":"Ω","bytes":[0,255]}))}
    #[test]
    fn families_skip_only_unacquired_streams_and_themes_include_overrides(){
        for (family,streams) in [("all",vec![0,1,2,3,4]),("themes",vec![0,4]),("translations",vec![1]),("images",vec![2]),("fileStore",vec![3])] {
            for stream in 0..=4 {let mut r=request(json!({"_pluginSettings":{"resourceFamily":family}}));r.cursor=Some(stream.to_string());let reply=handle(r);if streams.contains(&stream){let GuestReply::Action{state,..}=reply else{panic!("{family}/{stream}: {reply:?}")};assert_eq!(state["_pluginSettings"]["resourceFamily"],family);}else{assert!(matches!(reply,GuestReply::Inventory{authoritative:true,..}));}}
        }
        let mut r=request(json!({"phase":"themes","_pluginSettings":{"resourceFamily":"images"}}));r.response=Some(json!({"returnObj":{"Theme":[],"ThemeResource":[]}}));assert!(matches!(handle(r.clone()),GuestReply::Inventory{authoritative:false,diagnostics,..}if !diagnostics.is_empty()));
        r.response=None;r.error=Some(Diagnostic::new("DENIED","acquired refusal"));assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..}if diagnostics[0].code=="DENIED"));
    }
    #[test]
    fn frozen_selection_preserves_acquired_diagnostics_across_empty_independent_and_resume(){
        let settings=json!({"resourceFamily":"images"});let error=json!(Diagnostic::new("ACQUIRED","retained error"));let from=json!({"_pluginSettings":settings,"diagnostics":[error]});
        for independent in [false,true] {let reply=selected_reply(finish(vec![owner("Theme")],Some("1".into()),true,independent),&from).unwrap();let GuestReply::PreparedPage{state,next_state,..}=reply else{panic!()};assert_eq!(next_state["_pluginSettings"],settings);if !independent{assert!(state.get("_hostWorkItems").is_none());}let items=if independent {state["_hostWorkItems"].as_array().unwrap().clone()}else{vec![state]};for item in items{assert_eq!(item["_pluginSettings"],settings);assert!(matches!(handle(GuestRequest{operation:"materialize".into(),..request(item)}),GuestReply::Inventory{authoritative:false,diagnostics,..}if diagnostics.len()==1&&diagnostics[0].code=="ACQUIRED"));}}
        let GuestReply::ReadyRecords{records,state}=selected_reply(finish(vec![owner("ImageLibrary")],None,false,false),&from).unwrap()else{panic!()};assert_eq!(records,vec![owner("ImageLibrary")]);assert_eq!(state["_pluginSettings"],settings);assert!(matches!(handle(GuestRequest{operation:"materialize".into(),..request(state)}),GuestReply::Inventory{authoritative:false,diagnostics,..}if diagnostics.len()==1));
        let mut r=request(from);r.operation="acquire".into();assert!(matches!(handle(r),GuestReply::Failure{diagnostic}if diagnostic.message.contains("downloaded as complete groups")));
    }
    #[test]
    fn family_output_keeps_whole_aggregate_and_validates_acquired_theme_before_exclusion(){
        let all:Vec<_>=["Theme","StandaloneThemeResource","TranslationLibrary","ImageLibrary","FileStoreCustomizationResource"].into_iter().map(owner).collect();for family in ["all","themes","translations","images","fileStore"]{let expected:Vec<_>=all.iter().filter(|(id,_)|selected(id,family)).cloned().collect();let from=json!({"_pluginSettings":{"resourceFamily":family}});let GuestReply::PreparedPage{state,..}=selected_reply(finish(all.clone(),None,true,false),&from).unwrap()else{panic!()};let mut r=request(state);r.operation="materialize".into();let mut actual=vec![];loop{match handle(r.clone()){GuestReply::ReadyRecords{records,state}=>{actual.extend(records);r.state=state;},GuestReply::Inventory{authoritative:true,..}=>break,reply=>panic!("{reply:?}")}}assert_eq!(actual,expected);}
        for children in [Value::Null,json!({}),json!([{"ThemeID":"Other","SystemFlag":false}])]{let mut r=request(json!({"phase":"themes","_pluginSettings":{"resourceFamily":"images"}}));r.response=Some(json!({"returnObj":{"Theme":[{"ThemeID":"T","SystemFlag":false,"ThemeXML":"<Theme/>"}],"ThemeResource":children},"parameters":{"morePages":false}}));assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,..}));}
    }
}
