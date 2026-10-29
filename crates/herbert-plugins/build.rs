//! Package builds select one category per Cargo invocation. Ordinary tests retain all categories.
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};
// Kinetic applications and layers backup owner resolution delegates a personal owner to the Kinetic personalizations tables route.
// Retain its specification and all kind-guarded start/continue/append code.
fn dependency_kind(category: &str, kind: &str) -> bool {
    matches!(
        (category, kind),
        ("kinetic-applications-and-layers", "PersistedKineticPersonalization")
            | ("configurators", "PersistedKineticApplicationFile")
            | ("report-styles-and-layouts", "PersistedReportStyleRule")
    )
}
fn main() {
    println!("cargo:rerun-if-env-changed=HERBERT_PLUGIN_CATEGORY");
    let selected = env::var("HERBERT_PLUGIN_CATEGORY").ok();
    println!("cargo:rerun-if-changed=src/bin");
    let categories: BTreeSet<String> = fs::read_dir("src/bin")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    if let Some(ref category) = selected {
        assert!(
            categories.contains(category),
            "Unknown plugin build category"
        );
        println!("cargo:rustc-env=HERBERT_COMPILED_CATEGORY={category}");
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut kinds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in ["families.json", "tables.json"] {
        println!("cargo:rerun-if-changed=src/{name}");
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(format!("src/{name}")).unwrap()).unwrap();
        for row in &rows {
            kinds
                .entry(row["kind"].as_str().unwrap().into())
                .or_default()
                .insert(row["category"].as_str().unwrap().into());
        }
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| {
                selected.as_ref().is_none_or(|s| {
                    r["category"].as_str() == Some(s.as_str())
                        || dependency_kind(s, r["kind"].as_str().unwrap())
                })
            })
            .collect();
        fs::write(out.join(name), serde_json::to_vec(&rows).unwrap()).unwrap();
    }
    let mut code = String::from(
        "#[inline(always)] pub(crate) fn kind_enabled(kind: &str) -> bool { match kind {\n",
    );
    for (kind, categories) in kinds {
        let enabled = selected
            .as_ref()
            .is_none_or(|s| categories.contains(s) || dependency_kind(s, &kind));
        code.push_str(&format!("{kind:?} => {enabled},\n"));
    }
    code.push_str("_ => true, } }\n");
    fs::write(out.join("selected_kinds.rs"), code).unwrap();
}
