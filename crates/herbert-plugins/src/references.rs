//! Configuration references indexes explicit table references in collected JSON, never business rows.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const PAIRS: [(&str, &str); 5] = [
    ("DBSchemaName", "DBTableName"),
    ("SchemaName", "TableName"),
    ("SystemCode", "DataTableID"),
    ("RelatedToSchemaName", "RelatedToTable"),
    ("schema", "table"),
];

pub fn handle(request: GuestRequest) -> GuestReply {
    if matches!(request.operation.as_str(), "extract" | "processDefinition") {
        let Some(source) = request
            .identity
            .as_ref()
            .filter(|id| id.category != "configuration-references" && !id.kind.is_empty() && !id.keys.is_empty())
        else {
            return super::failure(
                "IDENTITY_REQUIRED",
                "Reference extraction needs a complete producer artifact identity.",
            );
        };
        let Some(definition) = request
            .state
            .get("definition")
            .filter(|d| d.is_object() || d.is_array() || d.is_string())
        else {
            return super::failure(
                "REFERENCE_INPUT_UNPARSED",
                "The producer supplied no structured JSON definition. Its source remains preserved; an empty reference set is not established.",
            );
        };
        let (records, mut diagnostics) = scan(source, definition);
        if request.state["_hostReferenceDiagnostics"] != true {
            diagnostics.clear();
        }
        return GuestReply::References {
            records,
            diagnostics,
        };
    }
    let references = request.state.get("references").and_then(Value::as_array);
    if request.operation == "discover" {
        let Some(references) = references else {
            return GuestReply::Inventory {
                identities: vec![],
                next_cursor: None,
                authoritative: false,
                diagnostics: vec![Diagnostic::new(
                    "REFERENCE_CONTEXT_REQUIRED",
                    "Reference discovery needs collected producer context. No business table is enumerated or downloaded.",
                )],
            };
        };
        let mut identities = BTreeMap::new();
        let mut records = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for reference in references {
            let Some(id) = reference
                .get("identity")
                .and_then(|v| serde_json::from_value::<Identity>(v.clone()).ok())
                .filter(valid_identity)
            else {
                diagnostics.push(Diagnostic::new("REFERENCE_CONTEXT_INVALID", "One supplied reference is invalid. Independent valid references remain available."));
                continue;
            };
            if !valid_record(reference, &id) {
                diagnostics.push(Diagnostic::new("REFERENCE_CONTEXT_INVALID","One supplied reference has invalid source provenance. Independent valid references remain available."));
                continue;
            }
            let key = serde_json::to_string(&id).unwrap();
            // Exact acquisition historically selects the first valid record.
            // Preserve that result while handing the complete local index to
            // the host once, rather than serializing it for every identity.
            records.entry(key.clone()).or_insert_with(|| (id.clone(), reference.clone()));
            identities.insert(key, id);
        }
        let producers = request.state.get("producerCount").and_then(Value::as_u64);
        let inspected = producers.map(|n| n > 0).unwrap_or(!references.is_empty());
        let partial = request.state["producerReferenceErrors"] == true || !diagnostics.is_empty();
        let summary = if !inspected {
            vec![Diagnostic::new(
                "REFERENCE_PRODUCERS_MISSING",
                "No inspected producer definitions are available. This empty index does not establish that configuration has no references.",
            )]
        } else if partial {
            vec![Diagnostic::new(
                "REFERENCE_PRODUCERS_PARTIAL",
                "Some producer definitions could not be inspected. Valid references remain available; review each producer's reference diagnostics.",
            )]
        } else {
            vec![]
        };
        diagnostics.extend(summary);
        let inventory = GuestReply::Inventory {
            identities: identities.into_values().collect(),
            next_cursor: None,
            authoritative: inspected && !partial,
            diagnostics,
        };
        return GuestReply::InventoryData {
            records: records.into_values().collect(),
            inventory: Box::new(inventory),
        };
    }
    if request.operation != "acquire" {
        return super::failure(
            "OPERATION_UNSUPPORTED",
            "Configuration references support local discovery, exact download and definition processing.",
        );
    }
    let Some(id) = request.identity.filter(valid_identity) else {
        return super::failure(
            "INVALID_IDENTITY",
            "Select an exact configuration table reference identity.",
        );
    };
    let Some(reference) = references.and_then(|rows| {
        rows.iter()
            .find(|v| v.get("identity") == Some(&json!(id)) && valid_record(v, &id))
    }) else {
        return super::failure(
            "REFERENCE_CONTEXT_REQUIRED",
            "The exact reference is absent from the supplied backup context.",
        );
    };
    GuestReply::Definition {
        identity: id,
        format: "definition-reference-json".into(),
        definition: reference.clone(),
        completeness: Completeness::Collected,
        diagnostics: vec![],
    }
}

fn valid_identity(id: &Identity) -> bool {
    id.category == "configuration-references"
        && matches!(
            id.kind.as_str(),
            "DatabaseTableReference" | "DataViewTableReference"
        )
        && id.keys.get("Table").is_some_and(|v| !v.trim().is_empty())
        && id.keys.get("Schema").is_none_or(|v| !v.trim().is_empty())
}

fn valid_record(record: &Value, id: &Identity) -> bool {
    record
        .get("occurrences")
        .and_then(Value::as_array)
        .is_some_and(|rows| {
            !rows.is_empty()
                && rows.iter().all(|row| {
                    let source = row
                        .get("source")
                        .and_then(|v| serde_json::from_value::<Identity>(v.clone()).ok());
                    let pointer = row.get("pointer").and_then(Value::as_str);
                    source.is_some_and(|s| {
                        s.category != "configuration-references"
                            && !s.kind.is_empty()
                            && !s.keys.is_empty()
                            && s.scope == id.scope
                    }) && pointer.is_some_and(|p| p.is_empty() || p.starts_with('/'))
                })
        })
}

/// This is a syntactic index. Names retain their exact spelling and source scope;
/// no database existence, effective access, SQL interpretation or closure is claimed.
pub fn scan(source: &Identity, definition: &Value) -> (Vec<Value>, Vec<Diagnostic>) {
    let mut scanner = Scanner {
        source,
        out: BTreeMap::new(),
        diagnostics: vec![],
        empty: 0,
        documents: 0,
    };
    scanner.document(definition, "", None);
    let mut coverage = Diagnostic::new(
        "REFERENCE_SCAN_COVERAGE",
        "Herbert listed the tables that saved definitions name explicitly. Names are kept exactly as written, with or without a schema prefix. Tables used only inside SQL or code, and business data, are not included.",
    );
    coverage.identity = Some(source.clone());
    coverage.context = BTreeMap::from([
        ("contract".into(), json!("explicit-table-references/2")),
        ("pairs".into(), json!(PAIRS)),
        ("references".into(), json!(scanner.out.len())),
        ("emptyPairs".into(), json!(scanner.empty)),
        ("embeddedDocuments".into(), json!(scanner.documents)),
        ("partial".into(), json!(!scanner.diagnostics.is_empty())),
    ]);
    scanner.diagnostics.push(coverage);
    (scanner.out.into_values().collect(), scanner.diagnostics)
}
struct Scanner<'a> {
    source: &'a Identity,
    out: BTreeMap<String, Value>,
    diagnostics: Vec<Diagnostic>,
    empty: usize,
    documents: usize,
}
impl Scanner<'_> {
    fn notice(&mut self, code: &str, path: &str, message: &str) {
        let mut d = Diagnostic::new(code, message);
        d.identity = Some(self.source.clone());
        d.context.insert("pointer".into(), json!(path));
        self.diagnostics.push(d);
    }
    fn record(
        &mut self,
        schema: Option<&str>,
        table: &str,
        path: &str,
        location: &Option<Value>,
        fields: Value,
    ) {
        if table.trim().is_empty() {
            return;
        }
        let mut keys = BTreeMap::from([("Table".into(), table.into())]);
        if let Some(schema) = schema.filter(|s| !s.trim().is_empty()) {
            keys.insert("Schema".into(), schema.into());
        }
        let id = Identity {
            category: "configuration-references".into(),
            kind: if fields.get("schema").is_some() {
                "DataViewTableReference"
            } else {
                "DatabaseTableReference"
            }
            .into(),
            scope: self.source.scope.clone(),
            keys,
        };
        let entry = self
            .out
            .entry(serde_json::to_string(&id).unwrap())
            .or_insert_with(|| json!({"identity":id,"occurrences":[]}));
        let occurrence =
            json!({"source":self.source,"pointer":path,"location":location,"fields":fields});
        let occurrences = entry["occurrences"].as_array_mut().unwrap();
        if !occurrences.contains(&occurrence) {
            occurrences.push(occurrence);
        }
    }
    fn qualified(&mut self, text: &str, path: &str, location: &Option<Value>, field: &str) {
        let (schema, table) = match text.split_once('.') {
            Some((s, t)) if !s.trim().is_empty() && !t.trim().is_empty() => (Some(s), t),
            _ => (None, text),
        };
        self.record(schema, table, path, location, json!({field:text}));
    }
    fn document(&mut self, definition: &Value, outer: &str, representation: Option<Value>) {
        let mut pending = vec![(definition, String::new())];
        while let Some((value, inner)) = pending.pop() {
            let path = if representation.is_some() {
                outer.to_owned()
            } else {
                inner.clone()
            };
            let location = representation
                .as_ref()
                .map(|r| json!({"representation":r,"pointer":inner}));
            match value {
                Value::Array(values) => {
                    for (i, v) in values.iter().enumerate().rev() {
                        pending.push((v, format!("{inner}/{i}")));
                    }
                }
                Value::Object(fields) => {
                    // Declared source views repeat original fields in the producer
                    // envelope. The original structured/text field is authoritative.
                    if inner == "/herbertSources" {
                        continue;
                    }
                    let mut paired = false;
                    for (schema_key, table_key) in PAIRS {
                        if !fields.contains_key(schema_key) || !fields.contains_key(table_key) {
                            continue;
                        }
                        if schema_key == "schema" && !inner.contains("/dataviews/") {
                            continue;
                        }
                        paired = true;
                        match(fields.get(schema_key).and_then(Value::as_str),fields.get(table_key).and_then(Value::as_str)){
                            // App Studio local/unbound views can retain a schema
                            // selection with an empty table. There is no table
                            // reference to index, not a malformed declaration.
                            (Some(s),Some(t))if t.trim().is_empty() && (s.trim().is_empty() || schema_key == "schema")=>self.empty+=1,
                            (Some(s),Some(t))if !t.trim().is_empty()=>self.record(Some(s),t,&path,&location,json!({schema_key:s,table_key:t})),
                            _=>self.notice("REFERENCE_PAIR_UNPARSED",&path,"A declared schema/table pair is incomplete or has unsupported value types. Independent valid references remain available.")
                        }
                    }
                    if fields.get("Source").and_then(Value::as_str) == Some("DB") {
                        if let (Some(schema), Some(table)) = (
                            fields.get("SystemCode").and_then(Value::as_str),
                            fields.get("BusinessObject").and_then(Value::as_str),
                        ) {
                            self.record(
                                Some(schema),
                                table,
                                &path,
                                &location,
                                json!({"Source":"DB","SystemCode":schema,"BusinessObject":table}),
                            );
                        }
                    }
                    if !paired {
                        for field in ["TableName", "BOMElementTableName", "dbTable"] {
                            if let Some(t) = fields.get(field).and_then(Value::as_str) {
                                self.record(None, t, &path, &location, json!({field:t}));
                            }
                        }
                    }
                    if let Some(t) = fields.get("FullTableName").and_then(Value::as_str) {
                        self.qualified(t, &path, &location, "FullTableName");
                    }
                    if inner.contains("/EfxRefTable/") || inner.ends_with("/EfxRefTable") {
                        if let Some(t) = fields.get("TableID").and_then(Value::as_str) {
                            self.qualified(t, &path, &location, "TableID");
                        }
                    }
                    let native_type = fields.get("ReferenceType");
                    if native_type == Some(&json!(1)) || native_type == Some(&json!("1")) {
                        if let Some(t) = fields.get("ReferenceID").and_then(Value::as_str) {
                            self.qualified(t, &path, &location, "ReferenceID");
                        }
                    }
                    for (key, v) in fields.iter().rev() {
                        pending.push((
                            v,
                            format!("{inner}/{}", key.replace('~', "~0").replace('/', "~1")),
                        ));
                    }
                }
                Value::String(text) => {
                    let trimmed = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
                    // XAML object references are markup extensions, not JSON
                    // documents or database ReferenceID declarations.
                    if representation.as_ref().is_some_and(|r| r["format"] == "xml")
                        && trimmed.strip_prefix("{x:Reference ").and_then(|s| s.strip_suffix('}'))
                            .is_some_and(|name| !name.trim().is_empty() && !name.contains(['{', '}']))
                    {
                        continue;
                    }
                    let format = if super::reference_documents::json_candidate(trimmed) {
                        Some("json")
                    } else if trimmed.starts_with('<')
                        && trimmed
                            .as_bytes()
                            .get(1)
                            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'?' || *b == b'!')
                    {
                        Some("xml")
                    } else {
                        None
                    };
                    if format.is_none() && inner.is_empty() && representation.is_none() {
                        self.notice("REFERENCE_INPUT_UNPARSED", &path, "The root definition is text without a recognized JSON/XML document. Its source remains preserved; an empty reference set is not established.");
                    }
                    if let Some(format) = format {
                        let parsed = if format == "json" {
                            super::reference_documents::json(trimmed)
                        } else {
                            super::reference_documents::xml(trimmed)
                        };
                        match parsed {
                            Ok(doc) => {
                                self.documents += 1;
                                self.document(
                                    &doc,
                                    &path,
                                    Some(json!({"format":format,"container":location})),
                                );
                            }
                            Err(_) => {
                                if PAIRS
                                    .iter()
                                    .any(|(s, t)| text.contains(s) && text.contains(t))
                                    || ["TableName", "BOMElementTableName", "dbTable", "FullTableName", "EfxRefTable", "ReferenceID"]
                                        .iter()
                                        .any(|field| text.contains(field))
                                    || ["Source", "SystemCode", "BusinessObject"].iter().all(|field| text.contains(field))
                                    || (inner.contains("/EfxRefTable") && text.contains("TableID"))
                                {
                                    self.notice("REFERENCE_CONTENT_UNPARSED",&path,"A locally retained definition document contains explicit table declarations but could not be parsed. Independent valid references remain available.");
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
pub fn extract(source: &Identity, definition: &Value) -> Vec<Value> {
    scan(source, definition).0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> Identity {
        Identity {
            category: "baqs-and-ubaqs".into(),
            kind: "BAQ".into(),
            scope: Scope::Company {
                company: "C".into(),
            },
            keys: BTreeMap::from([("QueryID".into(), "Owned".into())]),
        }
    }
    #[test]
    fn exact_names_scope_occurrences_and_partial_input_survive() {
        let d = json!({"a~/b":[{"DBSchemaName":"Erp","DBTableName":"Company"},{"DBSchemaName":"Erp","DBTableName":"Company"},{"DBSchemaName":"","DBTableName":""},{"DBSchemaName":"Erp","DBTableName":null}],"native":{"SchemaName":"Ice","TableName":"UD01"},"embedded":"<DBSchemaName>Erp</DBSchemaName><DBTableName>Part</DBTableName>"});
        let (records, diagnostics) = scan(&source(), &d);
        assert_eq!(records.len(), 2);
        for r in &records {
            assert_eq!(r["identity"]["scope"], json!(source().scope));
            for o in r["occurrences"].as_array().unwrap() {
                assert!(d.pointer(o["pointer"].as_str().unwrap()).is_some());
            }
        }
        assert_eq!(
            records
                .iter()
                .find(|r| r["identity"]["keys"]["Schema"] == "Erp")
                .unwrap()["occurrences"][0]["pointer"],
            "/a~0~1b/0"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_PAIR_UNPARSED")
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_SCAN_COVERAGE")
        );
    }
    #[test]
    fn unrelated_text_and_empty_structured_definition_have_coverage_without_warning() {
        let (records, diagnostics) = scan(
            &source(),
            &json!({"description":"Do useful work","other":[]}),
        );
        assert!(records.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "REFERENCE_SCAN_COVERAGE");
        assert_eq!(diagnostics[0].context["partial"], false);
    }

    #[test]
    fn local_documents_and_unqualified_names_preserve_declared_semantics() {
        let d = json!({"json":"{ /* native JSONC */ \"DBSchemaName\":\"Erp\",\"DBTableName\":\"Part\",}","xml":"<d><r SchemaName=\"Ice\" TableName=\"UD01\"/></d>","bad":"<a DBTableName=\"Part\" DBSchemaName=\"Erp\"/><a/>","target":{"TableName":"JobHead"},"view":"{\"dataviews\":[{\"schema\":\"Erp\",\"table\":\"Page1\"}]}","definition":{"EfxRefTable":[{"TableID":"ICE.ChgLog"}]}});
        let (records, diagnostics) = scan(&source(), &d);
        assert_eq!(records.len(), 5);
        assert!(
            records
                .iter()
                .any(|r| r["identity"]["kind"] == "DataViewTableReference")
        );
        assert!(
            records
                .iter()
                .any(|r| r["identity"]["keys"] == json!({"Table":"JobHead"}))
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_CONTENT_UNPARSED")
        );
        for record in &records {
            for o in record["occurrences"].as_array().unwrap() {
                assert!(d.pointer(o["pointer"].as_str().unwrap()).is_some());
            }
        }
    }
    fn request(op: &str, state: Value) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: op.into(),
            category: "configuration-references".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: None,
            state,
            response: None,
            error: None,
        }
    }
    #[test]
    fn missing_producers_and_invalid_context_keep_useful_partial_inventory() {
        let GuestReply::InventoryData { records, inventory } = handle(request("discover", json!({"references":[]}))) else { panic!() };
        assert!(records.is_empty());
        assert!(matches!(*inventory, GuestReply::Inventory { authoritative: false, .. }));
        let mut records = extract(
            &source(),
            &json!({"DBSchemaName":"Erp","DBTableName":"Part"}),
        );
        records.push(json!({"identity":{"wrong":"identity"}}));
        let GuestReply::InventoryData { records: bulk_records, inventory } = handle(request("discover", json!({"references":records}))) else { panic!() };
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            ..
        } = *inventory
        else {
            panic!()
        };
        assert_eq!(identities.len(), 1);
        assert_eq!(bulk_records.len(), 1);
        assert_eq!(bulk_records[0].1, records[0]);
        assert!(!authoritative);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_CONTEXT_INVALID")
        );
        let mut r = request("acquire", json!({"references":records}));
        r.identity = Some(identities[0].clone());
        assert!(matches!(handle(r), GuestReply::Definition { .. }));
    }
    #[test]
    fn bulk_discovery_preserves_every_exact_reference_and_partial_coverage() {
        let mut references = Vec::new();
        for index in 0..1004 {
            references.extend(extract(&source(), &json!({"SchemaName":"Erp","TableName":format!("Table{index}")})));
        }
        let mut input = references.clone();
        input.push(references[0].clone());
        input.push(json!({"identity":null}));
        let GuestReply::InventoryData { records, inventory } = handle(request("discover", json!({"references":input,"producerCount":1}))) else { panic!("Expected a complete local page") };
        assert_eq!(records.len(), references.len());
        let GuestReply::Inventory { identities, authoritative, diagnostics, next_cursor } = *inventory else { panic!() };
        assert!(!authoritative);
        assert!(next_cursor.is_none());
        assert!(diagnostics.iter().any(|d| d.code == "REFERENCE_CONTEXT_INVALID"));
        assert_eq!(identities, records.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>());
        for (identity, definition) in records {
            let mut exact = request("acquire", json!({"references":references}));
            exact.identity = Some(identity.clone());
            let GuestReply::Definition { identity: selected, definition: original, completeness, .. } = handle(exact) else { panic!() };
            assert_eq!(selected, identity);
            assert_eq!(original, definition);
            assert_eq!(completeness, Completeness::Collected);
        }
    }
    #[test]
    fn exact_scope_is_preserved_for_company_personal_shared_and_global() {
        for scope in [
            Scope::Global,
            Scope::Company {
                company: "C".into(),
            },
            Scope::Personal {
                company: "C".into(),
                owner: "a/b".into(),
            },
            Scope::SharedCompany {
                company: "C".into(),
                owner_company: "Owner".into(),
            },
        ] {
            let mut s = source();
            s.scope = scope.clone();
            let records = extract(&s, &json!({"DBSchemaName":"Erp","DBTableName":"Part"}));
            assert_eq!(records[0]["identity"]["scope"], json!(scope));
            assert_eq!(records[0]["occurrences"][0]["source"], json!(s));
        }
    }

    #[test]
    fn wave1_leading_jsonc_retains_source_provenance_and_valid_siblings() {
        for text in ["// lead\n{\"dbTable\":\"Erp.Part\"}", "/* lead */ [{\"dbTable\":\"Erp.Part\"},]", " \u{feff} /* lead */ {\"dbTable\":\"Erp.Part\"} // tail"] {
            let definition=json!({"embedded":text,"sibling":{"TableName":"JobHead"}});
            let original=definition.clone();
            let (records,diagnostics)=scan(&source(),&definition);
            assert_eq!(records.len(),2);
            assert_eq!(definition,original);
            assert!(diagnostics.iter().all(|d|d.code=="REFERENCE_SCAN_COVERAGE"));
            assert_eq!(diagnostics.last().unwrap().context["partial"],false);
            for record in records {for occurrence in record["occurrences"].as_array().unwrap(){assert!(definition.pointer(occurrence["pointer"].as_str().unwrap()).is_some());}}
        }
    }
    #[test]
    fn comments_in_sql_and_embedded_code_do_not_claim_unparsed_json() {
        let definition = json!({
            "DisplayPhrase":"/* query description */ SELECT TableName FROM Tables",
            "Body":serde_json::to_string(&json!({"Code":"// a code comment\nvar TableName = \"Part\";"})).unwrap(),
            "Workflow":"<Directive><Code>// comment\nvar TableName = &quot;Part&quot;;</Code></Directive>",
            "actual":{"SchemaName":"Erp","TableName":"Part"}
        });
        let (records, diagnostics) = scan(&source(), &definition);
        assert_eq!(records.len(), 1);
        assert!(diagnostics.iter().all(|d| d.code == "REFERENCE_SCAN_COVERAGE"));
        assert_eq!(diagnostics.last().unwrap().context["partial"], false);
    }
    #[test]
    fn unbound_dataview_schema_without_a_table_is_not_partial() {
        let definition = json!({"dataviews":[{"schema":"Erp","table":"","fields":[{"name":"LocalValue"}]},{"schema":"Erp","table":"Part"}]});
        let (records, diagnostics) = scan(&source(), &definition);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["identity"]["keys"]["Table"], "Part");
        assert!(diagnostics.iter().all(|d| d.code == "REFERENCE_SCAN_COVERAGE"));
    }
    #[test]
    fn xaml_object_references_do_not_claim_unparsed_table_declarations() {
        let definition=json!({"RuleBody":"<Workflow xmlns:x='http://schemas.microsoft.com/winfx/2006/xaml'><Node Value='{x:Reference __ReferenceID0}'/><Table SchemaName='Erp' TableName='Part'/></Workflow>"});
        let (records,diagnostics)=scan(&source(),&definition);
        assert_eq!(records.len(),1);
        assert!(diagnostics.iter().all(|d|d.code=="REFERENCE_SCAN_COVERAGE"));
        assert_eq!(diagnostics.last().unwrap().context["partial"],false);
    }
    #[test]
    fn wave1_malformed_supported_declarations_preserve_partial_coverage_and_peers() {
        let mut texts=vec![
            "{\"TableName\":", "{\"dbTable\":", "{\"BOMElementTableName\":", "{\"FullTableName\":",
            "{\"EfxRefTable\":[{\"TableID\":", "{\"ReferenceType\":1,\"ReferenceID\":",
            "{\"Source\":\"DB\",\"SystemCode\":\"Ice\",\"BusinessObject\":",
            "/* dbTable unterminated", "// lead\n{\"dbTable\":",
        ].into_iter().map(str::to_owned).collect::<Vec<_>>();
        for (schema,table) in PAIRS {texts.push(format!("{{\"{schema}\":\"Ice\",\"{table}\":"));}
        for text in texts {
            let definition=json!({"bad":text,"good":{"TableName":"Part"}});
            let original=definition.clone();let (records,diagnostics)=scan(&source(),&definition);
            assert_eq!(records.len(),1,"{text}");assert_eq!(definition,original);
            assert!(diagnostics.iter().any(|d|d.code=="REFERENCE_CONTENT_UNPARSED"),"{text}");
            assert_eq!(diagnostics.last().unwrap().context["partial"],true,"{text}");
        }
    }

}
