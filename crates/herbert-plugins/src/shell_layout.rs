//! Native home XML preserves source. Image identities come only from named fields.
//! ShellLayoutSvc stores DataSet.GetXml, not its caller-relative resolved view.
use herbert_contract::*;
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn owns(kind: &str) -> bool {
    matches!(
        kind,
        "PersistedShellLayout" | "PersistedShellLayoutPersonal" | "PersistedHomeUserOption"
    )
}
fn base_query(request: &GuestRequest, body: bool) -> HostAction {
    let mut parameters = BTreeMap::new();
    let mut criteria: Vec<_> = ["TenantID", "LayoutID", "HomePageType", "SubType"]
        .into_iter()
        .map(|field| {
            parameters.insert(
                field.into(),
                request.state["root"][format!("Base{field}")].clone(),
            );
            QueryCriterion {
                table: "BaseLayout".into(),
                field: field.into(),
                operator: "=".into(),
                parameter: field.into(),
            }
        })
        .collect();
    let mut fields = if body {
        vec![QueryField {
            table: "BaseLayout".into(),
            field: "*".into(),
            alias: "*".into(),
            expression: None,
        }]
    } else {
        [
            "TenantID",
            "LayoutID",
            "HomePageType",
            "SubType",
            "SystemFlag",
            "SysRowID",
            "SysRevID",
        ]
        .into_iter()
        .map(|field| QueryField {
            table: "BaseLayout".into(),
            field: field.into(),
            alias: field.into(),
            expression: None,
        })
        .collect()
    };
    if body {
        parameters.insert("custom".into(), json!(false));
        criteria.push(QueryCriterion {
            table: "BaseLayout".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "custom".into(),
        });
        for (field, alias) in [
            ("ShellHomePage", "HerbertHomeBytes"),
            ("ShellUserOptions", "HerbertOptionsBytes"),
            ("FavoriteItems", "HerbertFavoritesBytes"),
        ] {
            fields.push(QueryField {
                table: "BaseLayout".into(),
                field: field.into(),
                alias: alias.into(),
                expression: Some(format!(
                    "CONVERT(nvarchar(max), DATALENGTH(CONVERT(nvarchar(max), BaseLayout.{field})))"
                )),
            });
        }
    }
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: "ShellLayout".into(),
                alias: "BaseLayout".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields,
            joins: vec![],
            criteria,
            parameters,
            order: vec![],
            page_size: None,
            page: 1,
        },
    }
}
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    if request.state["root"]["BaseLayoutID"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        if [
            "BaseTenantID",
            "BaseLayoutID",
            "BaseHomePageType",
            "BaseSubType",
        ]
        .iter()
        .any(|f| !request.state["root"][*f].is_string())
        {
            issue(
                request,
                "SHELL_BASE_UNVERIFIED",
                "The personal layout lacks the complete linked base identity.",
            );
        } else {
            request.state["phase"] = json!("shellBaseMetadata");
            return Some(base_query(request, false));
        }
    }
    start_images(request)
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    if !matches!(
        request.state["phase"].as_str(),
        Some("shellBaseMetadata" | "shellBaseBody")
    ) {
        return complete_images(request);
    }
    let response = request.response.take().unwrap_or(Value::Null);
    if let Some(e) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(e));
        return start_images(request);
    }
    let rows = response["rows"].as_array();
    let row = rows.filter(|r| r.len() == 1).map(|r| &r[0]);
    let Some(row) = row.filter(|row| {
        ["TenantID", "LayoutID", "HomePageType", "SubType"]
            .iter()
            .all(|f| row[*f] == request.state["root"][format!("Base{f}")])
    }) else {
        issue(
            request,
            "SHELL_BASE_UNVERIFIED",
            "The linked base read did not return its one exact Epicor ID.",
        );
        return start_images(request);
    };
    if request.state["phase"] == "shellBaseMetadata" {
        request.state["shellBaseMetadata"] = row.clone();
        match row["SystemFlag"].as_bool() {
            Some(true) => {
                request.state["shellBaseExcluded"] = json!(true);
                return start_images(request);
            }
            Some(false) => {
                request.state["phase"] = json!("shellBaseBody");
                return Some(base_query(request, true));
            }
            None => {
                issue(
                    request,
                    "SHELL_BASE_UNVERIFIED",
                    "The linked base ownership flag is absent.",
                );
                return start_images(request);
            }
        }
    }
    if row["SystemFlag"] != false
        || ["SysRowID", "SysRevID"]
            .iter()
            .any(|f| row[*f] != request.state["shellBaseMetadata"][*f])
    {
        issue(
            request,
            "SHELL_BASE_CHANGED",
            "The linked custom base changed after its ownership check.",
        );
    } else {
        request.state["shellBase"] = row.clone();
    }
    start_images(request)
}
pub fn append(state: &Value, definition: &mut Value) {
    definition["homeResources"] = json!({"baseMetadata":state["shellBaseMetadata"],"baseSystemExcluded":state["shellBaseExcluded"]==true,"base":state["shellBase"],"images":state["shellImages"]});
    if !state["shellBase"].is_null() {
        if !definition["herbertSources"].is_object() {
            definition["herbertSources"] = json!({"textLengths":[],"chunkStreams":[]});
        }
        for (field, length) in [
            ("ShellHomePage", "HerbertHomeBytes"),
            ("ShellUserOptions", "HerbertOptionsBytes"),
            ("FavoriteItems", "HerbertFavoritesBytes"),
        ] {
            if state["shellBase"][field].is_string() {
                definition["herbertSources"]["textLengths"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"row":"/homeResources/base","field":field,"lengthField":length}));
            }
        }
    }
}
fn issue(request: &mut GuestRequest, code: &str, message: &str) {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
}
fn image_query(request: &GuestRequest) -> HostAction {
    let at = request.state["shellImageIndex"].as_u64().unwrap() as usize;
    let mut fields: Vec<_> = ["Company", "TenantID", "FileName", "SysRowID", "SysRevID"]
        .into_iter()
        .map(|f| QueryField {
            table: "Image".into(),
            field: f.into(),
            alias: f.into(),
            expression: None,
        })
        .collect();
    fields.push(QueryField {
        table: "Image".into(),
        field: "FileName".into(),
        alias: "HerbertContentBytes".into(),
        expression: Some("CONVERT(nvarchar(max), DATALENGTH(Image.Content))".into()),
    });
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: "FileStore".into(),
                alias: "Image".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields,
            joins: vec![],
            criteria: vec![QueryCriterion {
                table: "Image".into(),
                field: "SysRowID".into(),
                operator: "=".into(),
                parameter: "id".into(),
            }],
            parameters: BTreeMap::from([("id".into(), request.state["shellImageIds"][at].clone())]),
            order: vec![],
            page_size: None,
            page: 1,
        },
    }
}
fn next_image(request: &mut GuestRequest, advance: bool) -> Option<HostAction> {
    if advance {
        request.state["shellImageIndex"] =
            json!(request.state["shellImageIndex"].as_u64().unwrap() + 1);
    }
    if request.state["shellImageIndex"].as_u64().unwrap() as usize
        >= request.state["shellImageIds"].as_array().unwrap().len()
    {
        request.state["phase"] = json!("shellComplete");
        None
    } else {
        request.state["phase"] = json!("shellImageMetadata");
        Some(image_query(request))
    }
}
pub fn start_images(request: &mut GuestRequest) -> Option<HostAction> {
    let mut ids = BTreeSet::new();
    // Only source fields from the owned root and explicitly acquired base row.
    for row in [
        request.state["root"].clone(),
        request.state["shellBase"].clone(),
    ] {
        for field in ["ShellHomePage", "ShellUserOptions"] {
            if let Some(xml) = row[field].as_str() {
                match image_ids(xml) {
                    Ok(found) => ids.extend(found),
                    Err(e) => request.state["errors"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(e)),
                }
            }
        }
    }
    request.state["shellImageIds"] = json!(ids);
    request.state["shellImageIndex"] = json!(0);
    request.state["shellImages"] = json!([]);
    next_image(request, false)
}
fn image_metadata(request: &GuestRequest, response: &Value) -> Option<Value> {
    let rows = response["rows"].as_array()?;
    if rows.len() != 1 {
        return None;
    }
    let row = &rows[0];
    let at = request.state["shellImageIndex"].as_u64()? as usize;
    if !row["SysRowID"]
        .as_str()?
        .eq_ignore_ascii_case(request.state["shellImageIds"][at].as_str()?)
        || row["FileName"].as_str().is_none()
    {
        return None;
    }
    let company = row["Company"].as_str()?;
    if !company.is_empty() && company != request.company {
        return None;
    }
    let tenant = row["TenantID"].as_str()?;
    if let Some(owner_tenant) = request.state["root"]["TenantID"].as_str() {
        if !tenant.is_empty() && tenant != owner_tenant {
            return None;
        }
    }
    if row["HerbertContentBytes"].as_u64().is_none()
        && row["HerbertContentBytes"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .is_none()
    {
        return None;
    }
    Some(row.clone())
}
pub fn complete_images(request: &mut GuestRequest) -> Option<HostAction> {
    let response = request.response.take().unwrap_or(Value::Null);
    if let Some(e) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(e));
        return next_image(request, true);
    }
    match request.state["phase"].as_str().unwrap_or("") {
        "shellImageMetadata" => {
            let Some(metadata) = image_metadata(request, &response) else {
                issue(
                    request,
                    "SHELL_IMAGE_UNVERIFIED",
                    "The exact referenced image has no unique eligible FileStore metadata.",
                );
                return next_image(request, true);
            };
            request.state["shellImageMetadata"] = metadata.clone();
            request.state["shellImages"]
                .as_array_mut()
                .unwrap()
                .push(json!({"metadata":metadata}));
            request.state["phase"] = json!("shellImageBytes");
            Some(HostAction::SharedNativeRead {
                service: "Ice.Lib.FileStoreSvc".into(),
                method: "ReadAllBytes".into(),
                arguments: json!({"id":metadata["SysRowID"]}),
                export: true,
            })
        }
        "shellImageBytes" => {
            let errors = crate::file_store_resources::validate_bytes(
                &request.state["shellImageMetadata"],
                &response,
            );
            request.state["errors"]
                .as_array_mut()
                .unwrap()
                .extend(errors.into_iter().map(|e| json!(e)));
            request.state["shellImages"]
                .as_array_mut()
                .unwrap()
                .last_mut()
                .unwrap()["bytes"] = response;

            next_image(request, true)
        }
        _ => {
            issue(
                request,
                "INVALID_PLUGIN_STATE",
                "The home image saved progress is invalid.",
            );
            request.state["phase"] = json!("shellComplete");
            None
        }
    }
}

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("SHELL_XML_UNVERIFIED", message)
}
fn guid(value: &str) -> Result<Option<String>, Diagnostic> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                *c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
    {
        return Err(invalid("A home image reference is not an Epicor GUID."));
    }
    let value = value.to_ascii_lowercase();
    Ok((value != "00000000-0000-0000-0000-000000000000").then_some(value))
}

/// Parse the complete XML before issuing any image reads. Namespace aliases are
/// permitted. A same-named foreign element cannot introduce an image identity.
pub fn image_ids(xml: &str) -> Result<BTreeSet<String>, Diagnostic> {
    if xml.trim().is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = Vec::<(String, Vec<u8>)>::new();
    let mut root_seen = false;
    let mut root_ns = Vec::new();
    let mut value = String::new();
    let mut active = false;
    let mut row_fields = BTreeSet::new();
    let mut images = BTreeSet::new();
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| invalid("The retained home XML is malformed."))?;
        let namespace = match ns {
            ResolveResult::Bound(v) => v.as_ref().to_vec(),
            ResolveResult::Unbound => Vec::new(),
            ResolveResult::Unknown(_) => {
                return Err(invalid("The home XML uses an unbound namespace prefix."));
            }
        };
        match event {
            Event::Start(e) => {
                let name = String::from_utf8(e.local_name().as_ref().to_vec())
                    .map_err(|_| invalid("A home XML element name cannot be decoded."))?;
                if stack.is_empty() {
                    if root_seen {
                        return Err(invalid("The home XML has multiple document roots."));
                    }
                    root_seen = true;
                    root_ns = namespace.clone();
                }
                if active {
                    return Err(invalid("A home image GUID contains nested XML."));
                }
                if stack.len() == 1 {
                    row_fields.clear();
                }
                if stack.len() == 2 && namespace == root_ns && stack[1].1 == root_ns {
                    let target = matches!(
                        (stack[1].0.as_str(), name.as_str()),
                        ("HomeTile", "ImageRowID") | ("UserOptions", "AppLogoRowID")
                    );
                    if target {
                        if !row_fields.insert(name.clone()) {
                            return Err(invalid("A home row repeats its image identity field."));
                        }
                        active = true;
                        value.clear();
                    }
                }
                stack.push((name, namespace));
            }
            Event::Text(e) => {
                let decoded = e
                    .decode()
                    .map_err(|_| invalid("Home XML text cannot be decoded."))?;
                if active {
                    value.push_str(&decoded);
                } else if stack.is_empty() && !decoded.trim().is_empty() {
                    return Err(invalid("The home XML contains text outside its root."));
                }
            }
            Event::CData(e) => {
                let decoded = e
                    .decode()
                    .map_err(|_| invalid("Home XML CDATA cannot be decoded."))?;
                if active {
                    value.push_str(&decoded);
                } else if stack.is_empty() {
                    return Err(invalid("The home XML contains CDATA outside its root."));
                }
            }
            Event::GeneralRef(e) => {
                let name = e
                    .decode()
                    .map_err(|_| invalid("A home XML reference cannot be decoded."))?;
                let escaped = format!("&{name};");
                let decoded = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| invalid("The home XML contains an unresolved entity."))?;
                if active {
                    value.push_str(&decoded);
                }
            }
            Event::End(_) => {
                if active {
                    if let Some(id) = guid(&value)? {
                        images.insert(id);
                    }
                    active = false;
                }
                if stack.pop().is_none() {
                    return Err(invalid("The home XML closes an absent element."));
                }
            }
            Event::DocType(_) => {
                return Err(invalid(
                    "A home DataSet must not depend on a document type or external entity.",
                ));
            }
            Event::Eof => break,
            _ => (),
        }
    }
    if !root_seen || !stack.is_empty() {
        return Err(invalid("The retained home XML is incomplete."));
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "a83369f8-766c-4fc1-abde-080d5093b4de";
    #[test]
    fn public_options_read_keeps_other_owner_xml_and_length_declaration() {
        let xml = "<UserOptionsDataSet><UserOptions><SplashText1>café &amp; Ω</SplashText1></UserOptions></UserOptionsDataSet>";
        let row = json!({"UserID":"other-owner","HomePageType":"ERP","SubType":"kinetic","ShellUserOptions":xml,"HerbertOptionsBytes":xml.encode_utf16().count()*2,"SysRowID":"options-row","SysRevID":1});
        let mut r = request();
        r.state = json!({});
        r.response = None;
        r.identity.as_mut().unwrap().kind = "PersistedHomeUserOption".into();
        r.identity.as_mut().unwrap().keys = ["UserID", "HomePageType", "SubType"]
            .into_iter()
            .map(|f| (f.into(), row[f].as_str().unwrap().into()))
            .collect();
        // Acquisition first freezes the native recovery format (GetBaseLevel).
        let GuestReply::Action { action: HostAction::SharedNativeRead { method, .. }, state } =
            crate::handle("kinetic-personalizations", r.clone())
        else {
            panic!("recovery format read expected")
        };
        assert_eq!(method, "GetBaseLevel");
        r.state = state;
        r.response = Some(json!({"returnObj":{"_Major":4,"_Minor":3,"_Build":200,"_Revision":39}}));
        let GuestReply::Action {
            action: HostAction::Query { query } | HostAction::SharedQuery { query },
            state,
        } = crate::handle("kinetic-personalizations", r.clone())
        else {
            panic!()
        };
        assert_eq!(query.tables[0].table, "HomeUserOption");
        assert!(query.parameters.values().any(|v| v == "other-owner"));
        assert!(
            query
                .fields
                .iter()
                .any(|f| f.alias == "HerbertOptionsBytes")
        );
        r.state = state;
        r.response = Some(json!({"rows":[row]}));
        let GuestReply::Definition {
            identity,
            definition,
            completeness,
            diagnostics,
            ..
        } = crate::handle("kinetic-personalizations", r)
        else {
            panic!()
        };
        assert_eq!(
            identity.scope,
            Scope::Personal {
                company: "C".into(),
                owner: "other-owner".into()
            }
        );
        assert_eq!(definition["HomeUserOption"][0]["ShellUserOptions"], xml);
        assert_eq!(completeness, Completeness::Collected);
        assert!(diagnostics.is_empty());
        assert_eq!(
            definition["herbertSources"]["textLengths"][0]["lengthField"],
            "HerbertOptionsBytes"
        );
    }
    fn request() -> GuestRequest {
        let xml = format!(
            "<HomePageDataSet><HomeTile><ImageRowID>{ID}</ImageRowID></HomeTile></HomePageDataSet>"
        );
        let row = json!({"TenantID":"","UserID":"other-owner","LayoutID":"owned","HomePageType":"Modern","SubType":"","ShellHomePage":xml,"HerbertHomeBytes":xml.encode_utf16().count()*2,"BaseLayoutID":""});
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "kinetic-personalizations".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "kinetic-personalizations".into(),
                kind: "PersistedShellLayoutPersonal".into(),
                scope: Scope::Personal {
                    company: "C".into(),
                    owner: "other-owner".into(),
                },
                keys: ["TenantID", "UserID", "LayoutID", "HomePageType", "SubType"]
                    .into_iter()
                    .map(|f| (f.into(), row[f].as_str().unwrap().into()))
                    .collect(),
            }),
            cursor: None,
            page_size: Some(1),
            state: json!({"phase":"root"}),
            response: Some(json!({"rows":[row]})),
            error: None,
        }
    }
    #[test]
    fn personal_images_download_once_and_preserve_other_owner_scope() {
        for mode in ["ok", "missing", "foreign", "short"] {
            let mut r = request();
            let mut byte_reads = 0;
            loop {
                match crate::tables::handle(r.clone()) {
                    GuestReply::Action {
                        action: HostAction::Query { query } | HostAction::SharedQuery { query },
                        state,
                    } => {
                        assert_eq!(query.tables[0].table, "FileStore");
                        assert_eq!(query.parameters["id"], ID);
                        assert!(
                            query
                                .fields
                                .iter()
                                .all(|f| f.field != "Content" && f.field != "*")
                        );
                        let mut metadata = json!({"SysRowID":ID,"Company":"C","TenantID":"","FileName":"image.png","SysRevID":1,"HerbertContentBytes":"3"});
                        if mode == "foreign" {
                            metadata["Company"] = json!("OTHER");
                        }

                        r.state = state;
                        r.response =
                            Some(json!({"rows":if mode=="missing"{vec![]}else{vec![metadata]}}));
                    }
                    GuestReply::Action {
                        action:
                            HostAction::SharedNativeRead {
                                service,
                                method,
                                arguments,
                                export,
                            },
                        state,
                    } => {
                        assert_eq!(
                            (service.as_str(), method.as_str()),
                            ("Ice.Lib.FileStoreSvc", "ReadAllBytes")
                        );
                        assert!(export);
                        assert_eq!(arguments, json!({"id":ID}));
                        byte_reads += 1;
                        r.state = state;
                        r.response = Some(
                            json!({"returnObj":if mode=="short"{"YQ=="}else{"YWJj"},"parameters":{"fileName":"image.png"}}),
                        );
                    }
                    GuestReply::Definition {
                        definition,
                        completeness,
                        diagnostics,
                        ..
                    } => {
                        assert_eq!(
                            completeness,
                            if mode == "ok" {
                                Completeness::Collected
                            } else {
                                Completeness::Partial
                            }
                        );
                        assert_eq!(diagnostics.is_empty(), mode == "ok");
                        assert_eq!(
                            definition["ShellLayoutPersonal"][0]["UserID"],
                            "other-owner"
                        );
                        assert_eq!(
                            byte_reads,
                            if matches!(mode, "missing" | "foreign") {
                                0
                            } else {
                                1
                            }
                        );
                        break;
                    }
                    _ => panic!("Unexpected home acquisition reply"),
                }
            }
        }
    }
    #[test]
    fn linked_system_base_is_metadata_only_and_custom_base_is_filtered() {
        for system in [true, false] {
            let mut r = request();
            let row = &mut r.response.as_mut().unwrap()["rows"][0];
            row["ShellHomePage"] = json!("");
            row["HerbertHomeBytes"] = json!(0);
            for (f, v) in [
                ("BaseTenantID", ""),
                ("BaseLayoutID", "base"),
                ("BaseHomePageType", "Modern"),
                ("BaseSubType", ""),
            ] {
                row[f] = json!(v);
            }
            let mut reads = 0;
            loop {
                match crate::tables::handle(r.clone()) {
                    GuestReply::Action {
                        action: HostAction::Query { query } | HostAction::SharedQuery { query },
                        state,
                    } => {
                        assert_eq!(query.tables[0].table, "ShellLayout");
                        reads += 1;
                        let body = state["phase"] == "shellBaseBody";
                        assert_eq!(body, reads == 2);
                        if body {
                            assert!(!system);
                            assert_eq!(query.parameters["custom"], false);
                        } else {
                            assert!(query.fields.iter().all(|f| f.field != "*"));
                        }
                        let mut row = json!({"TenantID":"","LayoutID":"base","HomePageType":"Modern","SubType":"","SystemFlag":system,"SysRowID":"base-row","SysRevID":1});
                        if body {
                            row["ShellHomePage"] = json!("<HomePageDataSet/>");
                            row["HerbertHomeBytes"] = json!(36);
                        }
                        r.state = state;
                        r.response = Some(json!({"rows":[row]}));
                    }
                    GuestReply::Definition {
                        definition,
                        completeness,
                        ..
                    } => {
                        assert_eq!(completeness, Completeness::Collected);
                        assert_eq!(reads, if system { 1 } else { 2 });
                        assert_eq!(definition["homeResources"]["baseSystemExcluded"], system);
                        break;
                    }
                    _ => panic!("Unexpected linked base reply"),
                }
            }
        }
    }
    #[test]
    fn named_image_fields_namespace_aliases_and_shared_ids() {
        let xml = format!(
            "<d:HomePageDataSet xmlns:d='urn:home'><d:HomeTile><d:ImageRowID>{ID}</d:ImageRowID><d:Title>café &amp; Ω</d:Title></d:HomeTile><d:HomeTile><d:ImageRowID>{ID}</d:ImageRowID></d:HomeTile><d:UserOptions><d:AppLogoRowID>00000000-0000-0000-0000-000000000000</d:AppLogoRowID></d:UserOptions></d:HomePageDataSet>"
        );
        assert_eq!(image_ids(&xml).unwrap(), BTreeSet::from([ID.into()]));
        assert!(image_ids(&format!("<D xmlns='urn:home'><HomeTile xmlns='urn:other'><ImageRowID>{ID}</ImageRowID></HomeTile></D>")).unwrap().is_empty());
        assert!(
            image_ids("<D><HomeTile><ImageRowID/></HomeTile></D>")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn malformed_or_ambiguous_xml_cannot_schedule_image_reads() {
        for xml in [
            "<D><HomeTile>",
            "<D/><D/>",
            "<!DOCTYPE D><D/>",
            "<D><HomeTile><ImageRowID>bad</ImageRowID></HomeTile></D>",
            "<D><HomeTile><ImageRowID/><ImageRowID/></HomeTile></D>",
            "<D><HomeTile><ImageRowID><x/></ImageRowID></HomeTile></D>",
            "<p:D/>",
        ] {
            assert!(image_ids(xml).is_err(), "{xml}");
        }
    }
}
