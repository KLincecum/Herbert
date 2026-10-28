//! C20 native recovery files constructed from complete received definitions.
//! Posting XML follows PEABTExport's hierarchy, exclusions and COA substitutions.
use crate::xml_document::{Content, Element};
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn error(message: &str) -> Diagnostic {
    Diagnostic::new("BUSINESS_RECOVERY_INVALID", message)
}
fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_str(), b.as_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => a == b,
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn rows<'a>(data: &'a Value, table: &str) -> Result<&'a Vec<Value>, Diagnostic> {
    data[table]
        .as_array()
        .ok_or_else(|| error("A required posting table is missing."))
}
struct Graph<'a> {
    data: &'a Value,
    used: BTreeSet<(String, usize)>,
    index: BTreeMap<(String, String, String), Vec<usize>>,
}
fn index_value(v: &Value) -> String {
    v.as_str()
        .map(|s| format!("s:{}", s.to_ascii_lowercase()))
        .unwrap_or_else(|| v.to_string())
}
impl Graph<'_> {
    fn new(data: &Value) -> Graph<'_> {
        let mut index: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
        // Index relationship criteria once. Unindexed criteria still use the
        // normal exact filter, so this changes cost rather than semantics.
        for table in crate::business_pages::POSTING_TABLES {
            for (i, row) in data[*table].as_array().into_iter().flatten().enumerate() {
                for field in [
                    "ACTRevisionUID",
                    "ParentABTDocLineUID",
                    "ABTDocLineUID",
                    "ABTPostEntityUID",
                    "ABTQueryUID",
                    "BookID",
                    "ParentOperationUID",
                    "FunctionUID",
                    "FuncOperUID",
                    "RuleUID",
                    "vType",
                    "vUID",
                    "VLevel",
                ] {
                    if let Some(value) = row.get(field) {
                        index
                            .entry((table.to_string(), field.into(), index_value(value)))
                            .or_default()
                            .push(i);
                    }
                }
            }
        }
        Graph {
            data,
            used: BTreeSet::new(),
            index,
        }
    }
    fn selected(
        &self,
        table: &str,
        criteria: &[(&str, Value)],
        sort: &str,
    ) -> Result<Vec<(usize, Value)>, Diagnostic> {
        let stored = rows(self.data, table)?;
        let candidates = criteria
            .iter()
            .filter_map(|(f, v)| self.index.get(&(table.into(), (*f).into(), index_value(v))))
            .min_by_key(|ids| ids.len());
        let ids: Box<dyn Iterator<Item = usize> + '_> = if let Some(ids) = candidates {
            Box::new(ids.iter().copied())
        } else {
            Box::new(0..stored.len())
        };
        let mut result: Vec<_> = ids
            .filter_map(|i| {
                let r = &stored[i];
                criteria
                    .iter()
                    .all(|(f, v)| same(&r[*f], v))
                    .then(|| (i, r.clone()))
            })
            .collect();
        result.sort_by(|(_, a), (_, b)| text(&a[sort]).cmp(&text(&b[sort])));
        Ok(result)
    }
    fn node(
        &mut self,
        table: &str,
        index: usize,
        row: &Value,
        name: &str,
        keep: &[&str],
        exclude: &[&str],
        coa: Option<&str>,
    ) -> Result<Element, Diagnostic> {
        if !self.used.insert((table.into(), index)) {
            return Err(error(
                "Posting hierarchy repeats or cycles through a stored row.",
            ));
        }
        let mut result = Element::new(name);
        let mut row = row.clone();
        if let Some(coa) = coa {
            for field in [
                "Container",
                "Formula",
                "PRGText",
                "PRGContainer",
                "PRGPattern",
            ] {
                if let Some(s) = row[field].as_str() {
                    row[field] =
                        json!(s.replace(&format!("coaid=\"{coa}\""), "coaid=\"Current Book COA\""));
                }
            }
            for field in ["VarType", "ResultType", "ArgType"] {
                if row[field] == format!("Account--{coa}") {
                    row[field] = json!("Account--Current Book COA");
                }
            }
        }
        if matches!(
            table,
            "BROperation" | "BROperationCustom" | "BRFuncOperation"
        ) {
            row.as_object_mut().unwrap().remove("SequenceNumber");
            let id = row["FunctionUID"].as_i64().unwrap_or(0);
            let prg = if id >= 200 {
                rows(self.data, "BRFunction")?
                    .iter()
                    .find(|f| {
                        f["ACTRevisionUID"] == row["ACTRevisionUID"]
                            && (row["BookID"].is_null() || f["BookID"] == row["BookID"])
                            && f["FunctionUID"] == id
                    })
                    .and_then(|f| f["PRGName"].as_str())
                    .map(str::to_owned)
                    .unwrap_or(id.to_string())
            } else {
                id.to_string()
            };
            row["FunctionPRGName"] = json!(if id == 0 { String::new() } else { prg });
            if id >= 200 {
                if let Some(s) = row["PRGText"].as_str() {
                    row["PRGText"] = json!(s.replace(&format!("UF{id}"), "#FUNCTION#"));
                }
            }
        }
        for (field, value) in row
            .as_object()
            .ok_or_else(|| error("A posting row is not an object."))?
        {
            let skipped = exclude.contains(&field.as_str())
                || (table == "ACTBook" && field == "PartiallyUpdated")
                || (table == "BROperationCustom"
                    && ["CreatedOn", "CreatedBy", "ChangedOn", "ChangedBy"]
                        .contains(&field.as_str()))
                || [
                    "Company",
                    "BookID",
                    "SysRowID",
                    "SysRevID",
                    "BitFlag",
                    "RowMod",
                    "ConversionErrors",
                    "PendingConversion",
                    "PostAssemblyName",
                    "CanUseSysAssembly",
                    "IsLog",
                    "Modified",
                    "ManuallyUpdVer",
                    "UpdatePatchVer",
                    "NeedPrePostUpdate",
                    "ChildSequenceNumber",
                ]
                .contains(&field.as_str())
                || field.ends_with("UID")
                || field.contains("Loc")
                || field.contains("Prev")
                || field.starts_with("Herbert");
            if value.is_null() || (!keep.contains(&field.as_str()) && skipped) {
                continue;
            }
            if !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(error("A posting field has an invalid XML name."));
            }
            if !(value.is_string() || value.is_boolean() || value.is_number()) {
                return Err(error("A posting value is not scalar."));
            }
            let mut cell = Element::new(field);
            cell.content.push(Content::Text(escape(&text(value))));
            result.push(cell);
        }
        Ok(result)
    }
    fn operations(
        &mut self,
        table: &str,
        base: &[(&str, Value)],
        parent: i64,
        coa: &str,
    ) -> Result<Vec<Element>, Diagnostic> {
        let mut criteria = base.to_vec();
        criteria.push(("ParentOperationUID", json!(parent)));
        let mut selected = self.selected(table, &criteria, "ChildSequenceNumber")?;
        selected.sort_by_key(|(_, r)| r["ChildSequenceNumber"].as_i64().unwrap_or(0));
        let mut result = Vec::new();
        for (i, row) in selected {
            let mut n = self.node(
                table,
                i,
                &row,
                table,
                &[],
                if table == "BROperationCustom" {
                    &["IsDefault"]
                } else {
                    &[]
                },
                Some(coa),
            )?;
            let id = row["OperationUID"]
                .as_i64()
                .ok_or_else(|| error("Operation UID is missing."))?;
            for child in self.operations(table, base, id, coa)? {
                n.push(child);
            }
            result.push(n);
        }
        Ok(result)
    }
    fn doclines(&mut self, revision: &Value, parent: i64) -> Result<Vec<Element>, Diagnostic> {
        let mut result = Vec::new();
        for (i, row) in self.selected(
            "ABTDocLine",
            &[
                ("ACTRevisionUID", revision.clone()),
                ("ParentABTDocLineUID", json!(parent)),
            ],
            "Qualifier",
        )? {
            let mut n = self.node(
                "ABTDocLine",
                i,
                &row,
                "ABTDocLine",
                &["SequenceNumber"],
                &[],
                None,
            )?;
            let base = [
                ("ACTRevisionUID", revision.clone()),
                ("ABTDocLineUID", row["ABTDocLineUID"].clone()),
            ];
            for (i, entity) in self.selected("ABTPostEntity", &base, "Qualifier")? {
                let mut e = self.node(
                    "ABTPostEntity",
                    i,
                    &entity,
                    "ABTPostEntity",
                    &["SequenceNumber"],
                    &[],
                    None,
                )?;
                let mut criteria = base.to_vec();
                criteria.push(("ABTPostEntityUID", entity["ABTPostEntityUID"].clone()));
                for (i, r) in self.selected("ABTPostCode", &criteria, "Qualifier")? {
                    e.push(self.node("ABTPostCode", i, &r, "ABTPostCode", &[], &[], None)?);
                }
                n.push(e);
            }
            for (i, query) in self.selected("ABTQuery", &base, "BAQID")? {
                let mut q = self.node("ABTQuery", i, &query, "ABTQuery", &[], &[], None)?;
                let mut criteria = base.to_vec();
                criteria.push(("ABTQueryUID", query["ABTQueryUID"].clone()));
                for (i, r) in self.selected("ABTQParam", &criteria, "BAQParam")? {
                    q.push(self.node("ABTQParam", i, &r, "ABTQParam", &[], &[], None)?);
                }
                n.push(q);
            }
            for (i, r) in self.selected("ABTAmount", &base, "Qualifier")? {
                n.push(self.node("ABTAmount", i, &r, "ABTAmount", &[], &[], None)?);
            }
            for child in self.doclines(
                revision,
                row["ABTDocLineUID"]
                    .as_i64()
                    .ok_or_else(|| error("Document line UID is missing."))?,
            )? {
                n.push(child);
            }
            result.push(n);
        }
        Ok(result)
    }
    fn vars(
        &mut self,
        base: &[(&str, Value)],
        level: &str,
        id: Option<Value>,
        coa: &str,
    ) -> Result<Vec<Element>, Diagnostic> {
        let mut criteria = base.to_vec();
        criteria.push(("vType", json!(level)));
        if let Some(id) = id {
            criteria.push(("vUID", id));
        }
        let mut result = vec![];
        for (i, r) in self.selected("BookVar", &criteria, "VarName")? {
            result.push(self.node("BookVar", i, &r, "BookVar", &[], &[], Some(coa))?);
        }
        Ok(result)
    }
}
fn resolved_functions(data: &Value) -> Result<Value, Diagnostic> {
    let mut data = data.clone();
    for index in 0..rows(&data, "BRFunction")?.len() {
        let row = data["BRFunction"][index].clone();
        if row["Name"].as_str() != Some("") {
            continue;
        }
        let mut name = row["Formula"].as_str().unwrap_or("").to_owned();
        if name.is_empty() {
            continue;
        }
        while let Some(start) = name.find("</") {
            let end = name[start..]
                .find('>')
                .map(|i| start + i)
                .ok_or_else(|| error("Legacy function formula has an incomplete closing tag."))?;
            let tag = &name[start + 2..end];
            let closing = format!("</{tag}>");
            let opening = format!("<{tag}");
            let open = name
                .find(&opening)
                .ok_or_else(|| error("Legacy function formula has no opening tag."))?;
            let close = name[open..]
                .find('>')
                .map(|i| open + i)
                .ok_or_else(|| error("Legacy function formula has an incomplete opening tag."))?;
            name = name.replace(&name[open..=close], "").replace(&closing, "");
        }
        let mut program = name.clone();
        for token in [
            " ", ",", ";", "/", "\\\\", ".", " ", "(", ")", "--", ":", "-", "__ ",
        ] {
            program = program.replace(token, "_");
        }
        if program.len() <= 3
            && program.chars().all(|c| c.is_ascii_digit())
            && program.parse::<u32>().is_ok_and(|n| n <= 200)
        {
            program = format!("UF{program}");
        }
        let base_name = name.clone();
        let base_program = program.clone();
        let mut suffix = 0u64;
        while rows(&data, "BRFunction")?.iter().enumerate().any(|(i, r)| {
            i != index
                && r["ACTRevisionUID"] == row["ACTRevisionUID"]
                && r["BookID"] == row["BookID"]
                && r["PRGName"]
                    .as_str()
                    .is_some_and(|p| p.eq_ignore_ascii_case(&program))
        }) {
            suffix += 1;
            name = format!("{base_name}_{suffix}");
            program = format!("{base_program}_{suffix}");
        }
        let prior = row["PRGName"].as_str().unwrap_or("");
        let pattern = row["PRGPattern"].as_str().unwrap_or("");
        data["BRFunction"][index]["Name"] = json!(name);
        data["BRFunction"][index]["PRGName"] = json!(program);
        data["BRFunction"][index]["PRGPattern"] = json!(if prior.is_empty() {
            format!("{program}{pattern}")
        } else {
            pattern.replace(prior, &program)
        });
    }
    Ok(data)
}
pub fn posting_xml(source: &Value) -> Result<String, Diagnostic> {
    let resolved = resolved_functions(source)?;
    let data = &resolved;
    let roots = rows(data, "ACTType")?;
    if roots.len() != 1 {
        return Err(error("Posting recovery requires exactly one root."));
    }
    let mut g = Graph::new(data);
    let mut root = g.node("ACTType", 0, &roots[0], "ACTType", &[], &[], None)?;
    for (i, revision) in g.selected("ACTRevision", &[], "RevisionCode")? {
        let mut n = g.node("ACTRevision", i, &revision, "ACTRevision", &[], &[], None)?;
        let uid = revision["ACTRevisionUID"].clone();
        for line in g.doclines(&uid, 0)? {
            n.push(line);
        }
        for (i, book) in g.selected("ACTBook", &[("ACTRevisionUID", uid.clone())], "BookID")? {
            let coa = book["COAID"]
                .as_str()
                .ok_or_else(|| error("Book omitted its COA identity."))?;
            let mut adjusted = book.clone();
            let map_id = book["COAMapUID"].as_i64().unwrap_or(0);
            adjusted["COAMapName"] = if map_id > 0 {
                json!(
                    data["COAMap"]
                        .as_array()
                        .and_then(|r| r.iter().find(|r| r["COAMapUID"] == map_id))
                        .and_then(|r| r["DisplayName"].as_str())
                        .ok_or_else(|| error(
                            "A referenced COA mapping is missing; stored source is retained."
                        ))?
                )
            } else {
                json!("")
            };
            if adjusted["ABTVer"].as_str() == Some("") {
                adjusted["ABTVer"] = revision["ABTVer"].clone();
                adjusted["PatchABTVer"] = revision["PatchABTVer"].clone();
                adjusted["Package"] = revision["RType"].clone();
            }
            let mut b = g.node(
                "ACTBook",
                i,
                &adjusted,
                "Book",
                &["BookID"],
                &["BasePackage"],
                None,
            )?;
            let base = [
                ("ACTRevisionUID", uid.clone()),
                ("BookID", book["BookID"].clone()),
            ];
            for v in g.vars(&base, "Book", None, coa)? {
                b.push(v);
            }
            for (i, function) in g.selected("BRFunction", &base, "Name")? {
                let mut f = g.node(
                    "BRFunction",
                    i,
                    &function,
                    "BRFunction",
                    &[],
                    &[],
                    Some(coa),
                )?;
                let criteria = [
                    ("ACTRevisionUID", uid.clone()),
                    ("FunctionUID", function["FunctionUID"].clone()),
                ];
                let op_criteria = [
                    ("ACTRevisionUID", uid.clone()),
                    ("FuncOperUID", function["FunctionUID"].clone()),
                ];
                for o in g.operations("BRFuncOperation", &op_criteria, 0, coa)? {
                    f.push(o);
                }
                for (i, r) in g.selected("BRFuncArgs", &criteria, "ArgSeq")? {
                    f.push(g.node("BRFuncArgs", i, &r, "Argument", &[], &[], Some(coa))?);
                }
                for v in g.vars(
                    &[("ACTRevisionUID", uid.clone())],
                    "Function",
                    Some(function["FunctionUID"].clone()),
                    coa,
                )? {
                    f.push(v);
                }
                b.push(f);
            }
            for (i, rule) in g.selected("BookingRule", &base, "DisplayName")? {
                let mut r = g.node("BookingRule", i, &rule, "BookingRule", &[], &[], None)?;
                let criteria = [
                    ("ACTRevisionUID", uid.clone()),
                    ("RuleUID", rule["RuleUID"].clone()),
                ];
                for table in ["BROperation", "BROperationCustom"] {
                    for o in g.operations(table, &criteria, 0, coa)? {
                        r.push(o);
                    }
                }
                let mut bv = criteria.to_vec();
                bv.push(("VLevel", json!("RULE")));
                for (i, v) in g.selected("BVRule", &bv, "VRuleUID")? {
                    r.push(g.node("BVRule", i, &v, "BVRule", &["VRuleUID"], &[], None)?);
                }
                for v in g.vars(
                    &[("ACTRevisionUID", uid.clone())],
                    "Rule",
                    Some(rule["RuleUID"].clone()),
                    coa,
                )? {
                    r.push(v);
                }
                b.push(r);
            }
            n.push(b);
        }
        root.push(n);
    }
    for table in crate::business_pages::POSTING_TABLES {
        if rows(data, table)?
            .iter()
            .enumerate()
            .any(|(i, _)| !g.used.contains(&(table.to_string(), i)))
        {
            return Err(error(&format!(
                "A {table} row is outside the native hierarchy. Its source is retained; recovery XML was not emitted."
            )));
        }
    }
    let mut writer = quick_xml::Writer::new(Vec::new());
    root.write(&mut writer)?;
    String::from_utf8(writer.into_inner()).map_err(|_| error("Posting XML is not UTF-8."))
}
pub fn append(kind: &str, data: &mut Value) -> Result<(), Diagnostic> {
    if data["herbertNativePageExtensions"]
        .as_array()
        .is_some_and(|r| !r.is_empty())
    {
        return Err(error(
            "Native extension data is retained with the received page. Its per-definition import ownership could not be established.",
        ));
    }
    let (name, content) = if kind == "PostingRules" {
        ("PostingRules.xml", posting_xml(data)?)
    } else {
        let mut ds = data.clone();
        ds.as_object_mut()
            .ok_or_else(|| error("Missing native tableset."))?
            .retain(|k, _| !k.starts_with("herbert"));
        if kind == "BPMDataForm" {
            prepare_insert(&mut ds);
        }
        (
            if kind == "BPMDataForm" {
                "InfoPromptForm.json"
            } else {
                "StructuredOutput.json"
            },
            serde_json::to_string_pretty(&ds)
                .map_err(|_| error("Native tableset serialization failed."))?,
        )
    };
    data["herbertSources"] = json!({"files":[{"name":name,"text":content}]});
    if kind == "StructuredOutput" {
        let mut ds = data.clone();
        ds.as_object_mut()
            .unwrap()
            .retain(|k, _| !k.starts_with("herbert"));
        prepare_insert(&mut ds);
        data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"RestoreStructuredOutput.request.json","text":serde_json::to_string_pretty(&json!({"ds":ds,"continueProcessingOnError":false,"rollbackParentOnChildError":true})).unwrap()}));
    }
    Ok(())
}
fn prepare_insert(ds: &mut Value) {
    for rows in ds.as_object_mut().into_iter().flatten().map(|(_, v)| v) {
        for row in rows.as_array_mut().into_iter().flatten() {
            if row.is_object() {
                row["RowMod"] = json!("A");
                if row.get("SysRevID").is_some() {
                    row["SysRevID"] = json!(0);
                }
                if row.get("SysRowID").is_some() {
                    row["SysRowID"] = json!("00000000-0000-0000-0000-000000000000");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_posting_source_builds_native_recovery_without_service_calls() {
        let Ok(base) = std::env::var("HERBERT_C20_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let path = folder.join("posting-stored/rows.json");
            if !path.exists() {
                continue;
            }
            let data: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let xml = posting_xml(&data).unwrap();
            std::fs::write(folder.join("posting-local.xml"), &xml).unwrap();
            assert_eq!(crate::xml_document::parse(&xml).unwrap().name, "ACTType");
            let mut orphan = data.clone();
            let extra = orphan["ABTDocLine"][0].clone();
            orphan["ABTDocLine"].as_array_mut().unwrap().push(extra);
            let last = orphan["ABTDocLine"].as_array().unwrap().len() - 1;
            orphan["ABTDocLine"][last]["ParentABTDocLineUID"] = json!(999999);
            assert!(posting_xml(&orphan).is_err());
        }
    }
}
