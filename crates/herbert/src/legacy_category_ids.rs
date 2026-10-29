//! Retired category codes from Release 1 saved state. This frozen table is the only place
//! the host knows category IDs by name, and it is never extended: new categories have no
//! legacy form. Herbert never writes these codes. Stored records keep their bytes; callers
//! compare IDs with `same` and normalize only in-memory views (run history, refresh manifest).
use herbert_contract::{Diagnostic, Identity};
use serde_json::{json, Value};

/// (retired code, current slug). C05 and C07 are internal family codes.
const LEGACY: [(&str, &str); 26] = [
    ("C01", "ud-codes"),
    ("C02", "ud-schema"),
    ("C03", "extended-properties"),
    ("C04", "baqs-and-ubaqs"),
    ("C05", "ubaqs"),
    ("C06", "bpm-directives"),
    ("C07", "data-directives"),
    ("C08", "functions"),
    ("C09", "classic-customizations"),
    ("C10", "classic-personalizations"),
    ("C11", "classic-dashboards"),
    ("C12", "kinetic-applications-and-layers"),
    ("C13", "kinetic-personalizations"),
    ("C14", "searches-and-navigation"),
    ("C15", "menus-and-launch-definitions"),
    ("C16", "report-definitions"),
    ("C17", "report-styles-and-layouts"),
    ("C18", "report-behavior"),
    ("C19", "configurators"),
    ("C20", "business-rules-and-mappings"),
    ("C21", "custom-resources"),
    ("C22", "programs-and-extensions"),
    ("C24", "solutions-and-packages"),
    ("C25", "automation-and-integrations"),
    ("C26", "access-and-assignments"),
    ("C27", "configuration-references"),
];

/// Approved display renames (sentence-case display names). Views of old runs show the current name.
const DISPLAY_RENAMES: [(&str, &str); 1] = [("UD Codes", "UD codes")];

/// Journal maps keyed by category ID. Identity-hash-keyed maps are deliberately absent.
const CATEGORY_KEYED: [&str; 12] = [
    "categoryNames", "pluginSettings", "pluginHashes", "pluginSelectionContracts", "discovered", "cursors",
    "discoveryFinished", "categoryOutcomes", "pluginStates", "discoveryMessages", "pageBuffers", "capabilities",
];

/// The current ID for a stored category ID. Unknown and current IDs are returned unchanged.
pub(crate) fn current(id: &str) -> &str {
    LEGACY.iter().find(|(code, _)| *code == id).map_or(id, |(_, slug)| slug)
}

/// Whether two stored or current IDs name the same category.
pub(crate) fn same(a: &str, b: &str) -> bool {
    current(a) == current(b)
}

/// Sort key that keeps the established category order (the catalog order C01..C27 used) for every
/// user-visible list. Unknown or third-party IDs follow, in ID order. No contract field is added:
/// the order is the frozen table's order, which is the catalog order.
pub(crate) fn catalog_order(id: &str) -> (usize, String) {
    let id_now = current(id);
    let rank = LEGACY.iter().position(|(_, slug)| *slug == id_now).unwrap_or(LEGACY.len());
    (rank, id_now.to_owned())
}

/// The retired code for a current slug, used only to find Release 1 state by its stored key.
pub(crate) fn retired(slug: &str) -> Option<&'static str> {
    LEGACY.iter().find(|(_, s)| *s == slug).map(|(code, _)| *code)
}

/// MCP and CLI inputs must use current IDs. A retired code is refused with its replacement.
pub(crate) fn refuse_retired(id: &str) -> Result<(), Diagnostic> {
    match LEGACY.iter().find(|(code, _)| *code == id) {
        Some((code, slug)) => {
            let mut error = Diagnostic::new("LEGACY_CATEGORY_ID", format!("Category {code} was retired. Use {slug}."));
            error.context.insert("retiredCategory".into(), json!(code));
            error.context.insert("replacementCategory".into(), json!(slug));
            Err(error)
        }
        None => Ok(()),
    }
}

/// A plugin cannot claim an ID that is exactly a retired code: old saved state with that code
/// means the shipped category, so a third-party plugin with that ID would take over its history.
pub(crate) fn refuse_plugin_id(id: &str) -> Result<(), Diagnostic> {
    match LEGACY.iter().find(|(code, _)| *code == id) {
        Some((code, slug)) => {
            let mut error = Diagnostic::new("PLUGIN_ID_RETIRED", "This plugin ID is a retired Herbert category code and cannot be loaded. Ask the plugin author for a different ID.");
            error.context.insert("pluginId".into(), json!(code));
            error.context.insert("replacementCategory".into(), json!(slug));
            Err(error)
        }
        None => Ok(()),
    }
}

/// Refuse a command whose category-bearing fields use a retired code. Only the
/// command's own fields are inspected, at the top level and in a nested start
/// request (`run`, as serverChanges.preview sends it): a `category` string, a
/// `categories` array, the KEYS of `pluginSettings`, and each `identities[]`
/// entry's `category`. Nothing else is walked: setting values, `values`,
/// `selected`, native identity keys and any other user data are never read.
pub(crate) fn refuse_retired_arguments(arguments: &Value) -> Result<(), Diagnostic> {
    refuse_retired_fields(arguments)?;
    if let Some(run) = arguments.get("run") {
        refuse_retired_fields(run)?;
    }
    Ok(())
}

fn refuse_retired_fields(fields: &Value) -> Result<(), Diagnostic> {
    if let Some(id) = fields.get("category").and_then(Value::as_str) {
        refuse_retired(id)?;
    }
    for id in fields.get("categories").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        refuse_retired(id)?;
    }
    for id in fields.get("pluginSettings").and_then(Value::as_object).into_iter().flat_map(|settings| settings.keys()) {
        refuse_retired(id)?;
    }
    for identity in fields.get("identities").and_then(Value::as_array).into_iter().flatten() {
        if let Some(id) = identity.get("category").and_then(Value::as_str) {
            refuse_retired(id)?;
        }
    }
    Ok(())
}

pub(crate) fn normalize_identity(identity: &mut Identity) {
    let id = current(&identity.category);
    if id != identity.category {
        identity.category = id.to_owned();
    }
}

/// Re-key a category-keyed map in memory. A stored legacy key never overwrites a current one.
pub(crate) fn normalize_keys<V>(map: &mut std::collections::BTreeMap<String, V>) {
    let legacy: Vec<String> = map.keys().filter(|k| current(k) != k.as_str()).cloned().collect();
    for key in legacy {
        if let Some(value) = map.remove(&key) {
            map.entry(current(&key).to_owned()).or_insert(value);
        }
    }
}

/// Normalize a JSON view of saved state (run status, GUI, support export). Rewrites `category`
/// and `plugin` strings, `categories` arrays and the keys of category-keyed maps. Identity `keys`
/// values are user data and are never touched. Only views are passed here, never stored bytes.
pub(crate) fn normalize_view(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match (key.as_str(), &mut *item) {
                    ("category" | "plugin", Value::String(id)) => {
                        let id_now = current(id);
                        if id_now != id.as_str() {
                            *id = id_now.to_owned();
                        }
                        continue;
                    }
                    ("categories", Value::Array(ids)) => {
                        for id in ids.iter_mut() {
                            if let Value::String(text) = id {
                                *text = current(text).to_owned();
                            }
                        }
                        continue;
                    }
                    ("keys", _) => continue,
                    ("categoryNames", Value::Object(names)) => {
                        let legacy: Vec<String> = names.keys().filter(|k| current(k) != k.as_str()).cloned().collect();
                        for old in legacy {
                            if let Some(entry) = names.remove(&old) {
                                if !names.contains_key(current(&old)) {
                                    names.insert(current(&old).to_owned(), entry);
                                }
                            }
                        }
                        for name in names.values_mut() {
                            if let Some((_, now)) = DISPLAY_RENAMES.iter().find(|(old, _)| name.as_str() == Some(*old)) {
                                *name = json!(now);
                            }
                        }
                        continue;
                    }
                    (name, Value::Object(inner)) if CATEGORY_KEYED.contains(&name) => {
                        let legacy: Vec<String> = inner.keys().filter(|k| current(k) != k.as_str()).cloned().collect();
                        for old in legacy {
                            if let Some(entry) = inner.remove(&old) {
                                if !inner.contains_key(current(&old)) {
                                    inner.insert(current(&old).to_owned(), entry);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                normalize_view(item);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_view),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_codes_map_to_current_slugs_and_back() {
        assert_eq!(current("C01"), "ud-codes");
        assert_eq!(current("C05"), "ubaqs");
        assert_eq!(current("ud-codes"), "ud-codes");
        assert_eq!(current("vendor.collection.v2"), "vendor.collection.v2");
        assert!(same("C14", "searches-and-navigation"));
        assert!(!same("C14", "C15"));
        assert_eq!(retired("configuration-references"), Some("C27"));
        assert_eq!(retired("vendor.collection.v2"), None);
    }

    #[test]
    fn mcp_inputs_refuse_retired_codes_with_their_replacement() {
        let error = refuse_retired("C04").unwrap_err();
        assert_eq!(error.code, "LEGACY_CATEGORY_ID");
        assert!(error.message.contains("baqs-and-ubaqs"));
        assert_eq!(error.context["replacementCategory"], json!("baqs-and-ubaqs"));
        assert_eq!(error.context["retiredCategory"], json!("C04"));
        assert!(refuse_retired("baqs-and-ubaqs").is_ok());
        assert!(refuse_retired("C23").is_ok(), "C23 never shipped; it is an ordinary unknown ID");
    }

    #[test]
    fn argument_walk_checks_category_fields_of_the_command_and_its_run_but_not_native_key_values() {
        for refused in [
            json!({"categories":["ud-codes","C02"]}),
            json!({"pluginSettings":{"C01":{"codeTypeIdPrefix":"A"}}}),
            // retrieve_start identities
            json!({"identities":[{"category":"C14","kind":"NamedSearch","keys":{"SearchID":"x"}}]}),
            json!({"category":"C08","values":{}}),
            // serverChanges.preview nests a start request under run
            json!({"run":{"categories":["C03"]}}),
            json!({"run":{"pluginSettings":{"C12":{}}}}),
            json!({"run":{"identities":[{"category":"C16","kind":"Report","keys":{}}]}}),
        ] {
            let error = refuse_retired_arguments(&refused).unwrap_err();
            assert_eq!(error.code, "LEGACY_CATEGORY_ID", "{refused}");
            assert!(error.context.contains_key("replacementCategory"), "{refused}");
        }
        let accepted = json!({"categories":["ud-codes"],"pluginSettings":{"ud-codes":{"codeTypeIdPrefix":"C01"}},
            "identities":[{"category":"ud-codes","kind":"UDCodeType","keys":{"CodeTypeID":"C01"}}],"destination":"C:/C01",
            "run":{"categories":["ud-codes"],"identities":[{"category":"ud-codes","kind":"UDCodeType","keys":{"category":"C01"}}]}});
        assert!(refuse_retired_arguments(&accepted).is_ok());
    }

    #[test]
    fn plugin_settings_values_are_user_data_even_when_they_look_like_category_fields() {
        let accepted = json!({"pluginSettings":{"ud-codes":{"category":"C04","categories":["C01"],"pluginSettings":{"C02":{}}}},
            "run":{"pluginSettings":{"functions":{"category":"C08"}}}});
        assert!(refuse_retired_arguments(&accepted).is_ok(), "only pluginSettings keys name categories");
        assert!(refuse_retired_arguments(&json!({"run":{"pluginSettings":{"C08":{"category":"functions"}}}})).is_err());
    }

    #[test]
    fn plugin_configure_values_and_selected_are_user_data() {
        // plugin.configure: the category is checked; values and selections are not.
        for accepted in [
            json!({"category":"ud-codes","values":{"category":"C04"}}),
            json!({"category":"ud-codes","values":{"categories":["C01"],"pluginSettings":{"C02":{}}}}),
            json!({"category":"ud-codes","selected":[{"category":"C04"}],"values":{"identities":[{"category":"C04"}]}}),
            json!({"run":{"categories":["ud-codes"],"values":{"category":"C04"}}}),
            json!({"destination":"C:/C04","targets":[{"category":"C04"}]}),
        ] {
            assert!(refuse_retired_arguments(&accepted).is_ok(), "{accepted}");
        }
        assert!(refuse_retired_arguments(&json!({"category":"C04","values":{}})).is_err());
    }

    #[test]
    fn third_party_plugin_ids_that_equal_a_retired_code_are_refused() {
        for (code, slug) in LEGACY {
            let error = refuse_plugin_id(code).unwrap_err();
            assert_eq!(error.code, "PLUGIN_ID_RETIRED", "{code}");
            assert_eq!(error.context["replacementCategory"], *slug, "{code}");
        }
        for allowed in ["vendor.collection.v2", "C23", "c01", "C01x", "ud-codes"] {
            assert!(refuse_plugin_id(allowed).is_ok(), "{allowed}");
        }
    }

    #[test]
    fn normalized_maps_keep_an_existing_current_entry() {
        let mut map = std::collections::BTreeMap::from([
            ("C01".to_string(), 1),
            ("ud-codes".to_string(), 2),
            ("C02".to_string(), 3),
        ]);
        normalize_keys(&mut map);
        assert_eq!(map, std::collections::BTreeMap::from([("ud-codes".to_string(), 2), ("ud-schema".to_string(), 3)]));
    }

    #[test]
    fn release_1_journal_views_show_no_retired_code_and_keep_user_values() {
        let mut view = json!({"categories":["C01","C14"],"categoryNames":{"C01":"UD Codes","C14":"Searches and navigation"},
            "pluginSettings":{"C01":{"codeTypeIdPrefix":"C01"}},"pluginStates":{"C14":"running"},
            "artifacts":[{"identity":{"category":"C14","kind":"NamedSearch","keys":{"SearchID":"C14"}}}],
            "events":[{"event":"requestFailed","plugin":"C01"}]});
        normalize_view(&mut view);
        assert_eq!(view["categories"], json!(["ud-codes","searches-and-navigation"]));
        assert_eq!(view["categoryNames"]["ud-codes"], json!("UD codes"), "old runs show the current display name");
        assert_eq!(view["pluginSettings"]["ud-codes"]["codeTypeIdPrefix"], json!("C01"), "setting values are user data");
        assert_eq!(view["artifacts"][0]["identity"]["category"], json!("searches-and-navigation"));
        assert_eq!(view["artifacts"][0]["identity"]["keys"]["SearchID"], json!("C14"), "native key values are user data");
        assert_eq!(view["events"][0]["plugin"], json!("ud-codes"));
        let text = view.to_string();
        for (code, _) in LEGACY {
            assert!(!text.contains(&format!("\"{code}\":")), "{code} remains as a key");
        }
    }

    #[test]
    fn catalog_order_matches_the_shipping_catalog_and_puts_unknown_ids_last() {
        // The shipping catalog's own declaration order, read from its source.
        let catalog = include_str!("../../herbert-plugins/src/catalog.rs");
        let shipped: Vec<&str> = catalog
            .lines()
            .map(str::trim)
            .filter_map(|l| l.strip_prefix('"').and_then(|l| l.strip_suffix("\",")))
            .filter(|l| LEGACY.iter().any(|(_, slug)| slug == l))
            .collect();
        assert_eq!(shipped.len(), 24, "every shipping category appears once in the catalog");
        let mut shuffled: Vec<String> = shipped.iter().rev().map(|s| s.to_string()).collect();
        shuffled.insert(3, "vendor.collection.v2".into());
        shuffled.sort_by_key(|id| catalog_order(id));
        let mut expected: Vec<String> = shipped.iter().map(|s| s.to_string()).collect();
        expected.push("vendor.collection.v2".into());
        assert_eq!(shuffled, expected);
        assert_eq!(catalog_order("C14"), catalog_order("searches-and-navigation"), "stored codes sort with their slug");
    }

    #[test]
    fn the_table_is_one_to_one() {
        let slugs: std::collections::BTreeSet<_> = LEGACY.iter().map(|(_, s)| *s).collect();
        let codes: std::collections::BTreeSet<_> = LEGACY.iter().map(|(c, _)| *c).collect();
        assert_eq!((slugs.len(), codes.len()), (LEGACY.len(), LEGACY.len()));
        assert!(slugs.iter().all(|s| retired(s).is_some_and(|c| current(c) == *s)));
    }
}
