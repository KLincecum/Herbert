//! Reviewed native child identities, separate from category completion policy.
use herbert_contract::Diagnostic;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Contract {
    service: String,
    root: String,
    table: String,
    keys: BTreeMap<String, String>,
    links: BTreeMap<String, String>,
    unverified_parent: bool,
    lookup: bool,
}
fn gap(out: &mut Vec<Diagnostic>, code: &str, message: String) {
    out.push(Diagnostic::new(code, &message));
}
fn typed_key(value: &Value, native_type: &str) -> Option<String> {
    match native_type {
        "Edm.String" => value.as_str().map(str::to_owned),
        "Edm.Guid" => value.as_str().map(str::to_ascii_lowercase),
        "Edm.Int32" | "Edm.Int64" => value
            .as_i64()
            .or_else(|| value.as_str()?.parse::<i64>().ok())
            .map(|v| v.to_string()),
        "Edm.Boolean" => value.as_bool().map(|v| v.to_string()),
        _ => None,
    }
}
/// Split a full GetRows page once. Index children instead of scanning the page
/// again for every parent. Unknown populated tables require their own reader.
pub fn split_page(service: &str, root_table: &str, data: &Value) -> Option<Vec<Value>> {
    let contracts: Vec<Contract> =
        serde_json::from_str(include_str!("native-child-contracts.json")).ok()?;
    let roots = data.get(root_table)?.as_array()?;
    let skeleton: serde_json::Map<String, Value> = data
        .as_object()?
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if value.is_array() {
                    serde_json::json!([])
                } else {
                    value.clone()
                },
            )
        })
        .collect();
    let mut definitions: Vec<Value> = roots
        .iter()
        .map(|root| {
            let mut definition = Value::Object(skeleton.clone());
            definition[root_table] = serde_json::json!([root]);
            definition
        })
        .collect();
    for (table, value) in data.as_object()? {
        if table == root_table {
            continue;
        }
        let Some(rows) = value.as_array() else {
            continue;
        };
        if rows.is_empty() {
            continue;
        }
        let contract = contracts
            .iter()
            .find(|c| c.service == service && c.root == root_table && c.table == *table)?;
        if contract.unverified_parent && !contract.lookup {
            return None;
        }
        if contract.lookup {
            for definition in &mut definitions {
                definition[table] = value.clone();
            }
            continue;
        }
        if contract.links.is_empty() {
            return None;
        }
        let mut parents: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, root) in roots.iter().enumerate() {
            let key: Vec<_> = contract
                .links
                .values()
                .map(|field| root.get(field).filter(|v| !v.is_null()))
                .collect::<Option<_>>()?;
            parents
                .entry(serde_json::to_string(&key).ok()?)
                .or_default()
                .push(index);
            definitions[index][table] = serde_json::json!([]);
        }
        for row in rows {
            if crate::system_artifacts::marked(row) {
                continue;
            }
            let key: Vec<_> = contract
                .links
                .keys()
                .map(|field| row.get(field).filter(|v| !v.is_null()))
                .collect::<Option<_>>()?;
            let indexes = parents.get(&serde_json::to_string(&key).ok()?)?;
            for index in indexes {
                definitions[*index][table].as_array_mut()?.push(row.clone());
            }
        }
    }
    Some(definitions)
}
/// Native keys, including row discriminators, must be complete and unique.
/// Empty arrays are legitimate. Tableset lookups have keys but no root relation.
/// This validates returned rows. It does not prove independent source coverage.
pub fn validate(
    service: &str,
    root_table: &str,
    definition: &Value,
    declared: &[String],
) -> Vec<Diagnostic> {
    let contracts: Vec<Contract> =
        serde_json::from_str(include_str!("native-child-contracts.json"))
            .expect("Reviewed native child contracts must be valid");
    let mut out = Vec::new();
    let Some(roots) = definition.get(root_table).and_then(Value::as_array) else {
        gap(
            &mut out,
            "ROOT_UNVERIFIED",
            format!("The declared {root_table} root array is absent."),
        );
        return out;
    };
    if roots.len() != 1 || !roots[0].is_object() {
        gap(
            &mut out,
            "ROOT_UNVERIFIED",
            format!("The declared {root_table} root is not exactly one object."),
        );
        return out;
    }
    let root = &roots[0];
    for table in declared {
        let Some(rows) = definition.get(table).and_then(Value::as_array) else {
            gap(
                &mut out,
                "CHILD_TABLES_MISSING",
                format!("The required {table} child array is absent."),
            );
            continue;
        };
        let Some(contract) = contracts
            .iter()
            .find(|c| c.service == service && c.root == root_table && c.table == *table)
        else {
            gap(
                &mut out,
                "CHILD_CONTRACT_UNVERIFIED",
                format!("The {table} native child key contract is not declared."),
            );
            continue;
        };
        let mut seen = BTreeSet::new();
        for row in rows {
            let key = contract
                .keys
                .iter()
                .map(|(field, ty)| typed_key(row.get(field)?, ty))
                .collect::<Option<Vec<_>>>();
            if let Some(key) = key {
                if !seen.insert(key) {
                    gap(
                        &mut out,
                        "CHILD_IDENTITY_DUPLICATE",
                        format!("The {table} response repeats a complete native child key."),
                    );
                }
            } else {
                gap(
                    &mut out,
                    "CHILD_IDENTITY_UNVERIFIED",
                    format!("A {table} row omits or misrepresents a native child key field."),
                );
            }
            for (child_field, parent_field) in &contract.links {
                if row.get(child_field).is_none_or(Value::is_null)
                    || root.get(parent_field).is_none_or(Value::is_null)
                    || !(row.get(child_field) == root.get(parent_field)
                        // Native ZDataTableSvc returns ERP/Erp and ICE/Ice within one tableset.
                        // Dictionary identifiers also include XbSyst/XBSyst in native child rows.
                        // BAQDesignerSvc returns zHomepage_Job/zHomePage_Job in one native tableset.
                        || ((service == "Ice.BO.ZDataTableSvc" && matches!(child_field.as_str(), "SystemCode" | "DataTableID") || service == "Ice.BO.BAQDesignerSvc" && child_field == "QueryID")
                            && row.get(child_field).and_then(Value::as_str).zip(root.get(parent_field).and_then(Value::as_str))
                                .is_some_and(|(child, parent)| child.eq_ignore_ascii_case(parent))))
                {
                    gap(
                        &mut out,
                        "CHILD_IDENTITY_MISMATCH",
                        format!(
                            "A {table}.{child_field} value does not match its {root_table}.{parent_field} parent."
                        ),
                    );
                }
            }
        }
        if !rows.is_empty() && contract.unverified_parent && !contract.lookup {
            gap(
                &mut out,
                "CHILD_PARENT_UNVERIFIED",
                format!(
                    "The populated {table} table needs its source-backed indirect parent relationship."
                ),
            );
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn baq_query_key_casing_accepts_native_rows_but_not_foreign_query_or_company() {
        let mut data = json!({"DynamicQueryDesigner":[{"Company":"","QueryID":"zHomepage_Job"}],"QuerySortByDesigner":[{"Company":"","QueryID":"zHomePage_Job","SubQueryID":"00000000-0000-0000-0000-000000000001","TableID":"JobHead","FieldName":"JobNum"}]});
        let tables = vec!["QuerySortByDesigner".into()];
        assert!(
            validate(
                "Ice.BO.BAQDesignerSvc",
                "DynamicQueryDesigner",
                &data,
                &tables
            )
            .is_empty()
        );
        data["QuerySortByDesigner"][0]["QueryID"] = json!("other");
        assert!(
            validate(
                "Ice.BO.BAQDesignerSvc",
                "DynamicQueryDesigner",
                &data,
                &tables
            )
            .iter()
            .any(|d| d.code == "CHILD_IDENTITY_MISMATCH")
        );
        data["QuerySortByDesigner"][0]["QueryID"] = json!("zHomePage_Job");
        data["QuerySortByDesigner"][0]["Company"] = json!("foreign");
        assert!(
            validate(
                "Ice.BO.BAQDesignerSvc",
                "DynamicQueryDesigner",
                &data,
                &tables
            )
            .iter()
            .any(|d| d.code == "CHILD_IDENTITY_MISMATCH")
        );
    }
    #[test]
    fn complete_composite_keys_reject_duplicate_and_foreign_children() {
        let mut value = json!({"QuickSearch":[{"Company":"C","QuickSearchID":"Q","GlbCompany":""}],"QuickSearchCriteria":[]});
        let contracts: Vec<Contract> =
            serde_json::from_str(include_str!("native-child-contracts.json")).unwrap();
        let contract = contracts
            .iter()
            .find(|c| c.table == "QuickSearchCriteria")
            .unwrap();
        let mut child = serde_json::Map::new();
        for (key, ty) in &contract.keys {
            child.insert(
                key.clone(),
                match ty.as_str() {
                    "Edm.Int32" | "Edm.Int64" => json!(1),
                    "Edm.Boolean" => json!(false),
                    _ => json!("key"),
                },
            );
        }
        for (field, parent) in &contract.links {
            child.insert(field.clone(), value["QuickSearch"][0][parent].clone());
        }
        value["QuickSearchCriteria"] = json!([child]);
        let declared = vec!["QuickSearchCriteria".into()];
        assert!(validate("Ice.BO.QuickSearchSvc", "QuickSearch", &value, &declared).is_empty());
        let duplicate = value["QuickSearchCriteria"][0].clone();
        value["QuickSearchCriteria"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            validate("Ice.BO.QuickSearchSvc", "QuickSearch", &value, &declared)
                .iter()
                .any(|d| d.code == "CHILD_IDENTITY_DUPLICATE")
        );
        value["QuickSearchCriteria"][0]["QuickSearchID"] = json!("foreign");
        assert!(
            validate("Ice.BO.QuickSearchSvc", "QuickSearch", &value, &declared)
                .iter()
                .any(|d| d.code == "CHILD_IDENTITY_MISMATCH")
        );
    }
    #[test]
    fn schema_system_codes_accept_native_casing_but_reject_foreign_systems() {
        let mut value = serde_json::json!({"ZDataTable":[{"SystemCode":"ERP","DataTableID":"Part"}],"ZDataField":[{"SystemCode":"Erp","DataTableID":"Part","FieldName":"PartNum"}]});
        let declared = vec!["ZDataField".into()];
        assert!(validate("Ice.BO.ZDataTableSvc", "ZDataTable", &value, &declared).is_empty());
        value["ZDataField"][0]["SystemCode"] = serde_json::json!("Ice");
        assert!(
            validate("Ice.BO.ZDataTableSvc", "ZDataTable", &value, &declared)
                .iter()
                .any(|d| d.code == "CHILD_IDENTITY_MISMATCH")
        );
    }
    #[test]
    fn explicit_renamed_parent_and_lookup_contracts_are_preserved() {
        let contract: Vec<Contract> =
            serde_json::from_str(include_str!("native-child-contracts.json")).unwrap();
        let sub = contract.iter().find(|c| c.table == "ZSubDataSet").unwrap();
        assert_eq!(sub.links.get("ParentDataSetID").unwrap(), "DataSetID");
        assert!(!sub.unverified_parent);
        assert!(
            contract
                .iter()
                .filter(|c| c.lookup)
                .all(|c| c.service == "Ice.BO.LangNameSvc")
        );
        assert_eq!(
            contract
                .iter()
                .filter(|c| c.unverified_parent)
                .map(|c| c.table.as_str())
                .collect::<Vec<_>>(),
            vec!["RptDataSourceField"]
        );
    }
}
