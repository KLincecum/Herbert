//! Preserve readable authored text alongside native definitions and binary files.
use herbert_contract::{Diagnostic, FileRef, Identity};
use serde_json::{Value, json};

pub struct SourceFile {
    pub name: String,
    pub bytes: Vec<u8>,
    pub pointer: String,
    pub provenance: Option<Value>,
}
fn readable_pointer(pointer: &str) -> String {
    pointer
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let part = part.replace("~1", "/").replace("~0", "~");
            crate::output_layout::component(&part)
        })
        .collect::<Vec<_>>()
        .join("/")
}
/// A collector declares exact source pointers and parent relationships. Native
/// table names and collection scope remain in that collector, outside the host.
fn declared_sources(definition: &Value) -> (Vec<SourceFile>, Vec<Diagnostic>) {
    use serde::Deserialize;
    use std::collections::BTreeMap;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct TextLength {
        row: String,
        field: String,
        length_field: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ChunkStream {
        rows: String,
        root: String,
        links: BTreeMap<String, String>,
        length_field: String,
        encoding: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct TextFile {
        name: String,
        text: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct BinaryFile {
        name: String,
        base64: String,
        sha256: String,
        bytes: u64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct TextView {
        pointer: String,
        extension: String,
        text: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Sources {
        // The direct writer verifies and writes these native file references.
        // This pass only constructs readable text and inline binary views.
        #[serde(default, rename = "nativeFiles")]
        _native_files: Vec<serde_json::Value>,
        #[serde(default)]
        text_lengths: Vec<TextLength>,
        #[serde(default)]
        chunk_streams: Vec<ChunkStream>,
        #[serde(default)]
        files: Vec<TextFile>,
        #[serde(default)]
        binary_files: Vec<BinaryFile>,
        #[serde(default)]
        views: Vec<TextView>,
    }
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    let Some(value) = definition.get("herbertSources") else {
        return (files, diagnostics);
    };
    let Ok(sources) = serde_json::from_value::<Sources>(value.clone()) else {
        diagnostics.push(Diagnostic::new("SOURCE_CONTRACT_INVALID", "The collector's source-processing declaration is invalid; preserved native definitions remain available."));
        return (files, diagnostics);
    };
    for file in sources.files {
        if crate::storage::safe_relative(&file.name).is_err() {
            diagnostics.push(Diagnostic::new(
                "SOURCE_PATH_INVALID",
                "The plugin's readable source filename is not a safe relative path.",
            ));
            continue;
        }
        files.push(SourceFile {
            name: file.name,
            bytes: file.text.into_bytes(),
            pointer: String::new(),
            provenance: None,
        });
    }
    // The plugin builds the file. The host only verifies and writes its bytes.
    for file in sources.binary_files {
        use base64::Engine;
        if crate::storage::safe_relative(&file.name).is_err() {
            diagnostics.push(Diagnostic::new(
                "SOURCE_PATH_INVALID",
                "The plugin binary filename is not a safe relative path.",
            ));
            continue;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&file.base64) else {
            diagnostics.push(Diagnostic::new(
                "SOURCE_BINARY_INVALID",
                "The plugin binary file is not valid base64.",
            ));
            continue;
        };
        if bytes.len() as u64 != file.bytes || crate::storage::sha256(&bytes) != file.sha256 {
            diagnostics.push(Diagnostic::new(
                "SOURCE_BINARY_INVALID",
                "The plugin binary file does not match its byte count and SHA256.",
            ));
            continue;
        }
        files.push(SourceFile {
            name: file.name,
            bytes,
            pointer: String::new(),
            provenance: Some(json!({"representation":"plugin-built file","sha256":file.sha256})),
        });
    }
    for view in sources.views {
        let name = format!(
            "Code/{}.{}",
            readable_pointer(&view.pointer),
            view.extension
        );
        if view.pointer.is_empty()
            || !view.pointer.starts_with('/')
            || view.extension.is_empty()
            || view.extension.contains(['/', '\\'])
            || crate::storage::safe_relative(&name).is_err()
        {
            diagnostics.push(Diagnostic::new(
                "SOURCE_PATH_INVALID",
                "The plugin readable source view has an invalid pointer or extension.",
            ));
            continue;
        }
        files.push(SourceFile {
            name,
            bytes: view.text.into_bytes(),
            pointer: view.pointer,
            provenance: None,
        });
    }
    for source in sources.text_lengths {
        let row = definition.pointer(&source.row).unwrap_or(&Value::Null);
        if let Err(mut error) =
            crate::persisted_chunks::verify_text_length(row, &source.field, &source.length_field)
        {
            error.message = format!("{} Source: {}.{}.", error.message, source.row, source.field);
            diagnostics.push(error);
        }
    }
    for source in sources.chunk_streams {
        let root = definition.pointer(&source.root);
        let rows = definition.pointer(&source.rows).and_then(Value::as_array);
        let valid_parent = !source.links.is_empty()
            && root.is_some_and(Value::is_object)
            && rows.is_some_and(|rows| {
                rows.iter().all(|row| {
                    source.links.iter().all(|(field, parent)| {
                        row.get(field).is_some_and(|value| {
                            !value.is_null() && Some(value) == root.and_then(|r| r.get(parent))
                        })
                    })
                })
            });
        if !valid_parent
            || !["base64+bzip2", "base64+bzip2-or-xml"].contains(&source.encoding.as_str())
        {
            diagnostics.push(Diagnostic::new("PERSISTED_SOURCE_UNVERIFIED", format!("The declared stream {} has an absent or foreign parent, missing rows, or an unsupported encoding. Original rows remain preserved.", source.rows)));
            continue;
        }
        let reconstructed = if source.encoding == "base64+bzip2-or-xml" {
            crate::persisted_chunks::reconstruct_with_xml(rows.unwrap(), &source.length_field)
        } else {
            crate::persisted_chunks::reconstruct(rows.unwrap(), &source.length_field)
        };
        match reconstructed {
            Ok(Some(decoded)) => {
                let extension = std::str::from_utf8(&decoded.bytes)
                    .map(|text| {
                        if text
                            .trim_start_matches('\u{feff}')
                            .trim_start()
                            .starts_with('<')
                        {
                            "xml"
                        } else {
                            "txt"
                        }
                    })
                    .unwrap_or("bin");
                files.push(SourceFile {
                    name: format!("Code/{}.retained.{extension}", readable_pointer(&source.rows)),
                    pointer: source.rows.clone(),
                    provenance: Some(json!({"representation":"decoded retained chunk stream","encoding":decoded.encoding,"sequence":decoded.sequence,"root":source.root,"parentLinks":source.links,"sha256":crate::storage::sha256(&decoded.bytes)})),
                    bytes: decoded.bytes,
                });
            }
            Ok(None) => (),
            Err(mut error) => {
                error.message = format!("{} Source: {}.", error.message, source.rows);
                diagnostics.push(error);
            }
        }
    }
    (files, diagnostics)
}
pub fn output_error() -> Diagnostic {
    Diagnostic::new(
        "PROCESSING_FAILED",
        "Cannot preserve an authored source view; native acquisitions remain available.",
    )
}

fn export_identity(
    source: &Identity,
    definition: &Value,
    company: &str,
) -> Result<Identity, Diagnostic> {
    let Some(value) = definition.get("herbertExportIdentity") else {
        return Ok(source.clone());
    };
    let invalid = || {
        Diagnostic::new(
            "EXPORT_OWNER_INVALID",
            "The plugin's shared export owner must identify an artifact in this collection and company.",
        )
    };
    let owner: Identity = serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    let valid_scope = match &owner.scope {
        herbert_contract::Scope::Global => true,
        herbert_contract::Scope::SharedCompany { .. } => false,
        herbert_contract::Scope::Company { company: c }
        | herbert_contract::Scope::Personal { company: c, .. } => c.is_empty() || c == company,
    };
    if owner.category != source.category
        || owner.kind.is_empty()
        || owner.keys.is_empty()
        || !valid_scope
        || owner
            .keys
            .get("Company")
            .is_some_and(|c| !c.is_empty() && c != company)
    {
        return Err(invalid());
    }
    Ok(owner)
}

fn definition_filename(definition: &Value, default_stem: &str) -> Result<String, Diagnostic> {
    let Some(value) = definition.get("herbertDefinitionFile") else {
        return Ok(format!("{default_stem}.json"));
    };
    let invalid = || {
        Diagnostic::new(
            "DEFINITION_FILENAME_INVALID",
            "The plugin definition filename must be one safe JSON filename with a nonempty stem and no Windows invalid or reserved name.",
        )
    };
    let name = value.as_str().ok_or_else(invalid)?;
    let stem = name.strip_suffix(".json").ok_or_else(invalid)?;
    if stem.is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || name.ends_with([' ', '.'])
        || crate::storage::safe_relative(name).is_err()
    {
        return Err(invalid());
    }
    let device = stem
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let serial = device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"));
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&device.as_str())
        || serial.is_some_and(|v| {
            matches!(
                v,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    {
        return Err(invalid());
    }
    Ok(name.into())
}

fn shared_export(
    storage: &mut crate::storage::Storage,
    owner: &Identity,
    filename: &str,
    original: &FileRef,
) -> Result<(FileRef, Vec<Diagnostic>), Diagnostic> {
    let key = format!("{}:{filename}", crate::storage::identity_key(owner));
    let previous = storage
        .journal
        .shared_exports
        .get(&key)
        .cloned()
        .unwrap_or_default();
    for file in &previous {
        storage.verify(file)?;
        if (file.sha256 == original.sha256 && file.bytes == original.bytes)
            || crate::archives::same_contents(storage, file, original)
        {
            return Ok((file.clone(), vec![]));
        }
    }
    let mut diagnostics = Vec::new();
    let filename = if previous.is_empty() {
        filename.to_owned()
    } else {
        diagnostics.push(Diagnostic::new("EXPORT_CHANGED_DURING_RUN", "Independent reads returned different content for the same export. Both versions are retained."));
        let (stem, extension) = filename.rsplit_once('.').unwrap_or((filename, "bin"));
        format!("{stem} - {}.{extension}", original.sha256)
    };
    let mut bytes =
        std::fs::File::open(storage.confined(&original.path)?).map_err(|_| output_error())?;
    let (file, conflict) = storage.write_derived_stream(owner, &filename, &mut bytes)?;
    if conflict {
        diagnostics.push(Diagnostic::new(
            "USER_EDIT_CONFLICT",
            "Your edited export was preserved; the current server export is in Conflicts.",
        ));
    }
    storage
        .journal
        .shared_exports
        .entry(key)
        .or_default()
        .push(file.clone());
    storage.checkpoint()?;
    Ok((file, diagnostics))
}

/// The processing input is immutable and explicitly separate from native bytes.
/// This permits offline reprocessing while native receipts keep their own hashes.
pub fn write_views(
    storage: &mut crate::storage::Storage,
    identity: &Identity,
    definition: &Value,
) -> Result<(Vec<FileRef>, Vec<Diagnostic>), Diagnostic> {
    if let Some(group) = definition.get("herbertOutputGroup") {
        let group = group.as_str().ok_or_else(output_error)?;
        storage.group_artifact(identity, group)?;
    }
    if definition["herbertDirectOutput"] == true {
        return write_direct_views(storage, identity, definition);
    }
    let bytes = serde_json::to_vec_pretty(definition).map_err(|_| output_error())?;
    let input = storage.preserve_immutable(&bytes, "json", "processingInput")?;
    storage
        .journal
        .definition_views
        .insert(crate::storage::identity_key(identity), input.clone());
    storage.checkpoint()?;
    let mut files = vec![input];
    let mut diagnostics = Vec::new();
    let owner = export_identity(identity, definition, &storage.journal.company)?;
    let native = storage
        .journal
        .artifact_acquisitions
        .get(&crate::storage::identity_key(identity))
        .cloned()
        .unwrap_or_default();
    let (archive_files, archive_diagnostics) = crate::archives::views(storage, &owner, &native);
    files.extend(archive_files);
    diagnostics.extend(archive_diagnostics);
    let mut views = Vec::new();
    let name = crate::output_layout::artifact_name(identity);
    let definition_file = definition_filename(definition, &name)?;
    let mut exports = std::collections::BTreeMap::<String, usize>::new();
    let export_name = crate::output_layout::artifact_name(&owner);
    for original in &native {
        let extension = original.path.rsplit('.').next().unwrap_or("");
        if !matches!(
            extension,
            "baq"
                | "bpm"
                | "efxj"
                | "efxb"
                | "zip"
                | "xml"
                | "jsonc"
                | "rdl"
                | "png"
                | "jpg"
                | "dll"
        ) {
            continue;
        }
        storage.verify(original)?;
        let count = exports.entry(extension.to_owned()).or_default();
        *count += 1;
        let filename = if *count == 1 {
            format!("{export_name}.{extension}")
        } else {
            format!("{export_name} - {}.{extension}", *count)
        };
        let (file, export_diagnostics) = shared_export(storage, &owner, &filename, original)?;
        diagnostics.extend(export_diagnostics);
        files.push(file);
    }
    if exports.is_empty() {
        views.push(SourceFile {
            name: definition_file,
            bytes,
            pointer: String::new(),
            provenance: None,
        });
    }
    let (retained, source_diagnostics) = declared_sources(definition);
    // These files have passed the declared-source path, parent and integrity checks.
    // Keep reconstructed definitions as well as readable code.
    views.extend(retained);
    diagnostics.extend(source_diagnostics);
    for view in views {
        let source = storage.preserve_immutable(&view.bytes, "txt", "sourceView")?;
        match shared_export(storage, &owner, &view.name, &source) {
            Ok((file, source_diagnostics)) => {
                diagnostics.extend(source_diagnostics);
                files.push(file);
            }
            Err(mut error) => {
                error.identity = Some(identity.clone());
                diagnostics.push(error);
            }
        }
    }
    Ok((files, diagnostics))
}

/// Write native packages, readable JSON and authored text without intermediate
/// processing/source copies. Native responses remain the download evidence.
fn write_direct_views(
    storage: &mut crate::storage::Storage,
    identity: &Identity,
    definition: &Value,
) -> Result<(Vec<FileRef>, Vec<Diagnostic>), Diagnostic> {
    let prepared = prepare_direct_sources(identity, definition)?;
    write_prepared_direct_views(storage, identity, definition, prepared)
}

/// Prepare all inline outputs before the batch writer touches the filesystem.
pub(crate) fn prepare_direct_sources(identity: &Identity, definition: &Value)
    -> Result<(Vec<SourceFile>, Vec<Diagnostic>), Diagnostic> {
    let name = crate::output_layout::artifact_name(identity);
    let mut readable = definition.clone();
    if let Some(object) = readable.as_object_mut() {
        object.remove("herbertDirectOutput");
        object.remove("herbertOutputGroup");
        object.remove("herbertSources");
    }
    let bytes = serde_json::to_vec_pretty(&readable).map_err(|_| output_error())?;
    let (mut sources, source_diagnostics) = declared_sources(definition);
    sources.insert(
        0,
        SourceFile {
            name: format!("{name}.json"),
            bytes,
            pointer: String::new(),
            provenance: None,
        },
    );
    Ok((sources, source_diagnostics))
}

pub(crate) fn write_prepared_direct_views(
    storage: &crate::storage::Storage,
    identity: &Identity,
    definition: &Value,
    prepared: (Vec<SourceFile>, Vec<Diagnostic>),
) -> Result<(Vec<FileRef>, Vec<Diagnostic>), Diagnostic> {
    let native = storage
        .journal
        .artifact_acquisitions
        .get(&crate::storage::identity_key(identity))
        .cloned()
        .unwrap_or_default();
    let (mut files, mut diagnostics) = crate::archives::views(storage, identity, &native);
    let mut declared_native = std::collections::BTreeSet::new();
    // A paged collector can return an export obtained before an artifact was
    // selected. Verify that run-local file before copying it into the output.
    if let Some(declared) = definition.pointer("/herbertSources/nativeFiles") {
        for entry in declared.as_array().ok_or_else(output_error)? {
            let file: FileRef =
                serde_json::from_value(entry["file"].clone()).map_err(|_| output_error())?;
            let name = entry["name"].as_str().ok_or_else(output_error)?;
            crate::storage::safe_relative(name)?;
            storage.verify(&file)?;
            declared_native.insert((file.path.clone(), file.sha256.clone()));
            let mut input =
                std::fs::File::open(storage.confined(&file.path)?).map_err(|_| output_error())?;
            let (written, conflict) = storage.write_derived_stream(identity, name, &mut input)?;
            if conflict {
                diagnostics.push(Diagnostic::new("USER_EDIT_CONFLICT", "Your edited package was preserved; the current server version is in Conflicts."));
            }
            files.push(written);
        }
    }
    let name = crate::output_layout::artifact_name(identity);
    let mut exports = std::collections::BTreeMap::<String, usize>::new();
    for original in &native {
        // A verified explicit output already gives this retained file its native name.
        if declared_native.contains(&(original.path.clone(), original.sha256.clone())) {
            continue;
        }
        let extension = original.path.rsplit('.').next().unwrap_or("");
        if !matches!(
            extension,
            "baq"
                | "bpm"
                | "efxj"
                | "efxb"
                | "zip"
                | "xml"
                | "jsonc"
                | "rdl"
                | "png"
                | "jpg"
                | "dll"
        ) {
            continue;
        }
        storage.verify(original)?;
        let mut source =
            std::fs::File::open(storage.confined(&original.path)?).map_err(|_| output_error())?;
        let count = exports.entry(extension.into()).or_default();
        *count += 1;
        let filename = if *count == 1 {
            format!("{name}.{extension}")
        } else {
            format!("{name} - {count}.{extension}")
        };
        let (file, conflict) = storage.write_derived_stream(identity, &filename, &mut source)?;
        if conflict {
            diagnostics.push(Diagnostic::new(
                "USER_EDIT_CONFLICT",
                "Your edited package was preserved; the current server version is in Conflicts.",
            ));
        }
        files.push(file);
    }
    let (sources, source_diagnostics) = prepared;
    diagnostics.extend(source_diagnostics);
    for source in sources {
        let (file, conflict) = storage.write_derived(identity, &source.name, &source.bytes)?;
        if conflict {
            diagnostics.push(Diagnostic::new(
                "USER_EDIT_CONFLICT",
                "Different content already occupies this output path. The existing file is preserved; the current source version is in Conflicts.",
            ));
        }
        files.push(file);
    }
    Ok((files, diagnostics))
}

pub fn replace_references(
    storage: &mut crate::storage::Storage,
    source: &Identity,
    records: Vec<Value>,
) {
    let source_value = json!(source);
    let mut expected = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for record in &records {
        let key = serde_json::to_string(&record["identity"]).unwrap();
        let values = expected.entry(key).or_default();
        if let Some(rows) = record["occurrences"].as_array() {
            values.extend(
                rows.iter()
                    .filter(|v| v.get("source") == Some(&source_value))
                    .cloned(),
            );
        }
    }
    for values in expected.values_mut() {
        values.sort_by_key(|v| serde_json::to_string(v).unwrap());
        values.dedup();
    }
    let mut changed = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for record in &storage.journal.references {
        let key = serde_json::to_string(&record["identity"]).unwrap();
        seen.insert(key.clone());
        let mut old = record["occurrences"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.get("source") == Some(&source_value))
            .cloned()
            .collect::<Vec<_>>();
        old.sort_by_key(|v| serde_json::to_string(v).unwrap());
        old.dedup();
        if old.as_slice() != expected.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
            changed.insert(key);
        }
    }
    for (key, values) in &expected {
        if !values.is_empty() && !seen.contains(key) {
            changed.insert(key.clone());
        }
    }
    if changed.is_empty() {
        if storage.manifest.references != storage.journal.references {
            storage.manifest.references = storage.journal.references.clone();
        }
        return;
    }
    for record in &mut storage.journal.references {
        if let Some(values) = record.get_mut("occurrences").and_then(Value::as_array_mut) {
            values.retain(|v| v.get("source") != Some(&source_value));
        }
    }
    storage
        .journal
        .references
        .retain(|r| r["occurrences"].as_array().is_none_or(|v| !v.is_empty()));
    for record in records {
        if let Some(previous) = storage
            .journal
            .references
            .iter_mut()
            .find(|r| r.get("identity") == record.get("identity"))
        {
            if let (Some(target), Some(values)) = (
                previous
                    .get_mut("occurrences")
                    .and_then(Value::as_array_mut),
                record["occurrences"].as_array(),
            ) {
                for value in values {
                    if !target.contains(value) {
                        target.push(value.clone());
                    }
                }
            }
        } else {
            storage.journal.references.push(record);
        }
    }
    for record in &mut storage.journal.references {
        if let Some(values) = record.get_mut("occurrences").and_then(Value::as_array_mut) {
            values.sort_by_key(|v| serde_json::to_string(v).unwrap());
            values.dedup();
        }
    }
    storage
        .journal
        .references
        .sort_by_key(|r| serde_json::to_string(&r["identity"]).unwrap());
    storage.manifest.references = storage.journal.references.clone();
    // The index can advance before its consumer gets a write slot or before a
    // run is cancelled. Existing files must then be explicitly pending, not a
    // completed representation of different occurrence data.
    for value in changed {
        let Ok(id) = serde_json::from_str::<Identity>(&value) else {
            continue;
        };
        let key = crate::storage::identity_key(&id);
        let mark = |artifact: &mut herbert_contract::Artifact| {
            artifact.acquisition = herbert_contract::Completeness::Partial;
            artifact.processing = "partial".into();
            artifact.source_consistency = "notRecheckedAgainstCurrentIndex".into();
            if !artifact
                .diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_REFRESH_PENDING")
            {
                let mut d = Diagnostic::new(
                    "REFERENCE_REFRESH_PENDING",
                    "The preserved reference file has not been refreshed for the current source index.",
                );
                d.identity = Some(artifact.identity.clone());
                artifact.diagnostics.push(d);
            }
        };
        if let Some(artifact) = storage.manifest.artifacts.get_mut(&key) {
            mark(artifact);
        }
        if let Some(artifact) = storage
            .journal
            .artifacts
            .iter_mut()
            .find(|a| a.identity == id)
        {
            mark(artifact);
        }
    }
}

#[cfg(test)]
mod shared_export_tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        io::{Cursor, Write},
    };
    #[test]
    fn paged_native_file_is_verified_and_written_in_its_plugin_group() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("native-page".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let identity = owner();
        let file = storage.preserve_raw(b"native BPM bytes", "bpm").unwrap();
        let mut definition = json!({"herbertDirectOutput":true,"herbertOutputGroup":"Data directives",
            "definition":{"BpDirective":[]},"herbertSources":{"nativeFiles":[{"name":"table.bpm","file":file}]}});
        let (files, diagnostics) = write_views(&mut storage, &identity, &definition).unwrap();
        assert!(diagnostics.is_empty());
        let package = files
            .iter()
            .find(|f| f.path.ends_with("table.bpm"))
            .unwrap();
        assert!(package.path.contains("/Data directives/"));
        assert_eq!(
            std::fs::read(storage.confined(&package.path).unwrap()).unwrap(),
            b"native BPM bytes"
        );
        definition["herbertSources"]["nativeFiles"][0]["file"]["sha256"] = json!("wrong");
        assert!(write_views(&mut storage, &identity, &definition).is_err());
    }
    #[test]
    fn direct_query_output_preserves_package_json_code_and_user_edits_without_caches() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("direct-query".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let id = owner();
        let native = storage
            .preserve_raw(b"native package bytes", "baq")
            .unwrap();
        storage
            .journal
            .artifact_acquisitions
            .insert(crate::storage::identity_key(&id), vec![native]);
        let definition = json!({"herbertDirectOutput":true,"definition":{"Query":[{"Label":"Ω"}]},
            "supplements":{"diagrams":{"saved":true}},
            "herbertSources":{"files":[{"name":"Code/expression.txt","text":"a + b"}]}});
        let (files, errors) = write_views(&mut storage, &id, &definition).unwrap();
        assert!(errors.is_empty());
        assert_eq!(files.len(), 3);
        let package = files.iter().find(|f| f.path.ends_with(".baq")).unwrap();
        assert_eq!(
            std::fs::read(temp.path().join(&package.path)).unwrap(),
            b"native package bytes"
        );
        let output = files.iter().find(|f| f.path.ends_with(".json")).unwrap();
        let data: Value =
            serde_json::from_slice(&std::fs::read(temp.path().join(&output.path)).unwrap())
                .unwrap();
        assert_eq!(data["definition"]["Query"][0]["Label"], "Ω");
        assert!(data.get("herbertDirectOutput").is_none());
        assert!(data["supplements"].get("ExportBaq").is_none());
        assert!(storage.journal.definition_views.is_empty());
        assert!(
            !files
                .iter()
                .any(|f| f.role == "processingInput" || f.role == "sourceView")
        );
        std::fs::write(temp.path().join(&output.path), b"user edit").unwrap();
        let (_, errors) = write_views(&mut storage, &id, &definition).unwrap();
        assert!(errors.iter().any(|e| e.code == "USER_EDIT_CONFLICT"));
        assert_eq!(
            std::fs::read(temp.path().join(&output.path)).unwrap(),
            b"user edit"
        );
    }
    #[test]
    fn plugin_declared_views_preserve_text_without_host_field_inference() {
        let text = "// Unicode Ω\r\nreturn 42;";
        let definition = json!({"Body":"must not infer", "FunctionID":"not a host rule", "herbertSources":{"views":[
            {"pointer":"/Custom~1Table/0/AuthorText","extension":"cs","text":text},
            {"pointer":"/Other","extension":"../outside","text":"reject"}
        ]}});
        let (views, errors) = declared_sources(&definition);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "Code/Custom%2FTable/0/AuthorText.cs");
        assert_eq!(views[0].bytes, text.as_bytes());
        assert_eq!(errors[0].code, "SOURCE_PATH_INVALID");
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("declared-view".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let (files, errors) = write_views(&mut storage, &owner(), &definition).unwrap();
        assert_eq!(errors.len(), 1);
        let file = files
            .iter()
            .find(|file| file.role == "derived" && file.path.ends_with("AuthorText.cs"))
            .unwrap();
        assert_eq!(
            std::fs::read(temp.path().join(&file.path)).unwrap(),
            text.as_bytes()
        );
        assert!(!files.iter().any(|file| file.path.ends_with("Body.cs")));
    }
    #[test]
    fn explicit_native_name_suppresses_only_the_same_retained_auto_copy() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("named-native".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let id = owner();
        let named = storage.preserve_raw(b"named binary", "dll").unwrap();
        let other = storage.preserve_raw(b"other binary", "dll").unwrap();
        storage.journal.artifact_acquisitions.insert(crate::storage::identity_key(&id), vec![named.clone(), other]);
        let definition = json!({"herbertDirectOutput":true,"definition":{"Id":"named"},"herbertSources":{"nativeFiles":[{"name":"original.dll","file":named}]}});
        let (files, diagnostics) = write_views(&mut storage, &id, &definition).unwrap();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let binaries: Vec<_> = files.iter().filter(|f| f.path.ends_with(".dll")).collect();
        assert_eq!(binaries.len(), 2);
        assert_eq!(binaries.iter().filter(|f| f.path.ends_with("original.dll")).count(), 1);
        let mut contents: Vec<_> = binaries.iter().map(|f| std::fs::read(temp.path().join(&f.path)).unwrap()).collect();
        contents.sort();
        assert_eq!(contents, vec![b"named binary".to_vec(), b"other binary".to_vec()]);
    }
    fn owner() -> Identity {
        Identity {
            category: "collection".into(),
            kind: "Query".into(),
            scope: herbert_contract::Scope::Company {
                company: "C".into(),
            },
            keys: BTreeMap::from([("QueryID".into(), "Named query".into())]),
        }
    }
    #[test]
    fn plugin_built_binary_files_require_integrity_and_confined_names() {
        use base64::Engine;
        let bytes = b"PK\x00\xffbinary";
        let valid = json!({"name":"Query.baq","base64":base64::engine::general_purpose::STANDARD.encode(bytes),"sha256":crate::storage::sha256(bytes),"bytes":bytes.len()});
        let mut bad_hash = valid.clone();
        bad_hash["sha256"] = json!("wrong");
        let mut outside = valid.clone();
        outside["name"] = json!("../Query.baq");
        let definition = json!({"herbertDirectOutput":true,"herbertSources":{"binaryFiles":[valid,bad_hash,outside]}});
        let (sources, errors) = declared_sources(&definition);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].bytes, bytes);
        assert_eq!(errors.len(), 2);
        let temp = tempfile::tempdir().unwrap();
        let mut storage = crate::storage::Storage::open(
            temp.path(),
            false,
            crate::storage::new_journal("local-file".into()),
        )
        .unwrap();
        let (files, _) = write_views(&mut storage, &owner(), &definition).unwrap();
        let file = files
            .iter()
            .find(|file| file.path.ends_with("Query.baq"))
            .unwrap();
        assert_eq!(std::fs::read(temp.path().join(&file.path)).unwrap(), bytes);
    }
    #[test]
    fn shared_definition_filenames_preserve_distinct_sources_and_real_changes() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("definitions".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let root = owner();
        let mut design = root.clone();
        design.kind = "Design".into();
        let first = json!({"authored":"root"});
        let second = json!({"authored":"design Ω","herbertExportIdentity":root,"herbertDefinitionFile":"Design.json"});
        let (one, errors) = write_views(&mut storage, &root, &first).unwrap();
        assert!(errors.is_empty());
        let (two, errors) = write_views(&mut storage, &design, &second).unwrap();
        assert!(errors.is_empty());
        let one = one.iter().find(|f| f.role == "derived").unwrap();
        let two = two.iter().find(|f| f.role == "derived").unwrap();
        assert!(one.path.ends_with("/Named query.json"));
        assert!(two.path.ends_with("/Design.json"));
        assert_eq!(
            one.path.rsplit_once('/').unwrap().0,
            two.path.rsplit_once('/').unwrap().0
        );
        assert_eq!(
            std::fs::read(temp.path().join(&one.path)).unwrap(),
            serde_json::to_vec_pretty(&first).unwrap()
        );
        assert_eq!(
            std::fs::read(temp.path().join(&two.path)).unwrap(),
            serde_json::to_vec_pretty(&second).unwrap()
        );
        let mut changed = second;
        changed["authored"] = json!("actually changed");
        let (_, errors) = write_views(&mut storage, &design, &changed).unwrap();
        assert!(errors.iter().any(|e| e.code == "EXPORT_CHANGED_DURING_RUN"));
    }
    #[test]
    fn invalid_definition_filename_never_falls_back_or_escapes() {
        for name in [
            json!(null),
            json!(42),
            json!(""),
            json!(".json"),
            json!("../x.json"),
            json!("/x.json"),
            json!("C:x.json"),
            json!("dir\\x.json"),
            json!("x?.json"),
            json!("CON.json"),
            json!("CON .json"),
            json!("LPT9.json"),
            json!("COM¹.json"),
            json!("CONOUT$.json"),
            json!("x.json "),
            json!("x.txt"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut journal = crate::storage::new_journal("invalid-definition".into());
            journal.company = "C".into();
            let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
            let error = write_views(
                &mut storage,
                &owner(),
                &json!({"herbertDefinitionFile":name}),
            )
            .unwrap_err();
            assert_eq!(error.code, "DEFINITION_FILENAME_INVALID", "{name}");
            assert!(storage.journal.shared_exports.is_empty());
        }
        assert_eq!(
            definition_filename(&json!({"herbertDefinitionFile":"100% Ω.json"}), "unused").unwrap(),
            "100% Ω.json"
        );
    }
    fn archive(second: u8, body: &[u8]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "definition",
                zip::write::SimpleFileOptions::default().last_modified_time(
                    zip::DateTime::from_date_and_time(2026, 10, 18, 1, 0, second).unwrap(),
                ),
            )
            .unwrap();
        writer.write_all(body).unwrap();
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn shared_exports_ignore_only_container_changes_and_preserve_real_changes() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("shared".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let first = storage
            .preserve_raw(&archive(0, b"authored"), "zip")
            .unwrap();
        let repeated = storage
            .preserve_raw(&archive(2, b"authored"), "zip")
            .unwrap();
        let changed = storage
            .preserve_raw(&archive(4, b"changed"), "zip")
            .unwrap();
        assert_ne!(first.sha256, repeated.sha256);
        let (one, errors) =
            shared_export(&mut storage, &owner(), "Named query.zip", &first).unwrap();
        assert!(errors.is_empty());
        let (two, errors) =
            shared_export(&mut storage, &owner(), "Named query.zip", &repeated).unwrap();
        assert_eq!(one, two);
        assert!(errors.is_empty());
        let (three, errors) =
            shared_export(&mut storage, &owner(), "Named query.zip", &changed).unwrap();
        assert_ne!(one.path, three.path);
        assert_eq!(errors[0].code, "EXPORT_CHANGED_DURING_RUN");
        for file in [&first, &repeated, &changed, &one, &three] {
            storage.verify(file).unwrap();
        }
        assert_eq!(
            storage
                .journal
                .shared_exports
                .values()
                .next()
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn shared_owner_cannot_cross_collection_or_company() {
        let source = owner();
        assert_eq!(export_identity(&source, &json!({}), "C").unwrap(), source);
        assert_eq!(
            export_identity(&source, &json!({"herbertExportIdentity":source}), "C").unwrap(),
            source
        );
        for owner in [
            json!({"category":"other","kind":"Query","scope":{"type":"global"},"keys":{"QueryID":"q"}}),
            json!({"category":"collection","kind":"Query","scope":{"type":"company","company":"other"},"keys":{"QueryID":"q"}}),
            Value::Null,
        ] {
            assert!(
                export_identity(&source, &json!({"herbertExportIdentity":owner}), "C").is_err()
            );
        }
    }
    #[test]
    fn declared_text_files_preserve_language_and_reject_escaping_paths() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("declared-text".into());
        journal.company = "C".into();
        let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
        let definition = json!({"herbertSources":{"files":[
            {"name":"Code/Script.vb","text":"Imports System\r\n' Unicode Ω\r\n"},
            {"name":"Code/Query.sql","text":"select 'Ω';"},
            {"name":"../escaped.txt","text":"must not be written"}
        ]}});
        let (files, diagnostics) = write_views(&mut storage, &owner(), &definition).unwrap();
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.code == "SOURCE_PATH_INVALID")
                .count(),
            1
        );
        for (suffix, expected) in [
            ("Code/Script.vb", "Imports System\r\n' Unicode Ω\r\n"),
            ("Code/Query.sql", "select 'Ω';"),
        ] {
            let file = files
                .iter()
                .find(|f| f.role == "derived" && f.path.ends_with(suffix))
                .expect("declared readable file");
            assert_eq!(
                std::fs::read(temp.path().join(&file.path)).unwrap(),
                expected.as_bytes()
            );
            storage.verify(file).unwrap();
        }
        assert!(!files.iter().any(|f| f.path.contains("escaped.txt")));
    }
    #[test]
    fn reconstructed_streams_reach_download_output() {
        use base64::Engine as _;
        for bytes in [
            b"<Legacy>caf\xc3\xa9 \xce\xa9</Legacy>".to_vec(),
            vec![0, 255, 1, 128],
        ] {
            let mut encoder =
                bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
            encoder.write_all(&bytes).unwrap();
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());
            let definition = json!({"root":{"Company":"C"},"chunks":[{"Company":"C","SeqNum":1,"Chunk":encoded,"Length":encoded.encode_utf16().count()*2}],"herbertSources":{"chunkStreams":[{"root":"/root","rows":"/chunks","links":{"Company":"Company"},"lengthField":"Length","encoding":"base64+bzip2"}]}});
            let temp = tempfile::tempdir().unwrap();
            let mut journal = crate::storage::new_journal("reconstructed".into());
            journal.company = "C".into();
            let mut storage = crate::storage::Storage::open(temp.path(), false, journal).unwrap();
            let (files, errors) = write_views(&mut storage, &owner(), &definition).unwrap();
            assert!(errors.is_empty(), "{errors:?}");
            let suffix = if bytes[0] == b'<' {
                ".retained.xml"
            } else {
                ".retained.bin"
            };
            let file = files
                .iter()
                .find(|f| f.role == "derived" && f.path.ends_with(suffix))
                .unwrap();
            assert_eq!(std::fs::read(temp.path().join(&file.path)).unwrap(), bytes);
            storage.verify(file).unwrap();
        }
    }
    #[test]
    fn plain_files_share_only_exact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let mut storage = crate::storage::Storage::open(
            temp.path(),
            false,
            crate::storage::new_journal("plain".into()),
        )
        .unwrap();
        let first = storage.preserve_raw(b"first", "xml").unwrap();
        let second = storage.preserve_raw(b"second", "xml").unwrap();
        let (one, _) = shared_export(&mut storage, &owner(), "definition.xml", &first).unwrap();
        let (two, errors) =
            shared_export(&mut storage, &owner(), "definition.xml", &second).unwrap();
        assert_ne!(one.path, two.path);
        assert_eq!(errors.len(), 1);
        storage.verify(&one).unwrap();
        storage.verify(&two).unwrap();
    }
}

#[cfg(test)]
mod classic_source_tests {
    use super::*;
    #[test]
    fn dashboard_assembly_is_a_preserved_source_with_measured_text_length() {
        let definition = json!({"DashBdDef":[{"DashboardAssembly":"AAEC/w==","HerbertDashboardAssemblyBytes":"16"}],"herbertSources":{"textLengths":[{"row":"/DashBdDef/0","field":"DashboardAssembly","lengthField":"HerbertDashboardAssemblyBytes"}],"chunkStreams":[]}});
        assert!(declared_sources(&definition).1.is_empty());
        let mut truncated = definition.clone();
        truncated["DashBdDef"][0]["DashboardAssembly"] = json!("AAEC");
        assert!(!declared_sources(&truncated).1.is_empty());
    }
    #[test]
    fn rebased_native_classic_length_checks_are_not_skipped() {
        let mut definition = json!({"supplements":{"persisted":{"XXXDef":[{"Content":"<x/>","HerbertContentBytes":"8"}],"XXXChunk":[]}},"herbertSources":{"textLengths":[{"row":"/supplements/persisted/XXXDef/0","field":"Content","lengthField":"HerbertContentBytes"}],"chunkStreams":[{"rows":"/supplements/persisted/XXXChunk","root":"/supplements/persisted/XXXDef/0","links":{"Company":"Company"},"lengthField":"HerbertChunkBytes","encoding":"base64+bzip2"}]}});
        assert!(declared_sources(&definition).1.is_empty());
        definition["supplements"]["persisted"]["XXXDef"][0]["HerbertContentBytes"] = json!("20");
        assert!(!declared_sources(&definition).1.is_empty());
    }
}

#[cfg(test)]
mod classic_chunk_declaration_tests {
    use super::*;
    #[test]
    fn declared_native_xml_stream_is_reconstructed_with_parent_and_length_proof() {
        let definition = json!({"XXXDef":[{"Company":"C"}],"XXXChunk":[{"Company":"C","SeqNum":9,"Chunk":"<Legacy/>","HerbertChunkBytes":"18"}],"herbertSources":{"textLengths":[],"chunkStreams":[{"rows":"/XXXChunk","root":"/XXXDef/0","links":{"Company":"Company"},"lengthField":"HerbertChunkBytes","encoding":"base64+bzip2-or-xml"}]}});
        let (files, errors) = declared_sources(&definition);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bytes, b"<Legacy/>");
        assert_eq!(
            files[0].provenance.as_ref().unwrap()["encoding"],
            "xml-text"
        );
        let mut foreign = definition;
        foreign["XXXChunk"][0]["Company"] = json!("OTHER");
        assert!(!declared_sources(&foreign).1.is_empty());
    }
}

#[cfg(test)]
mod human_output_tests {
    use super::*;
    use crate::storage::{Storage, identity_key, new_journal};
    use std::{
        collections::BTreeMap,
        io::{Cursor, Write},
    };
    #[test]
    fn native_export_is_exact_and_only_readable_code_is_expanded() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = new_journal("human-output".into());
        journal.category_names.insert("C04".into(), "BAQs".into());
        let mut store = Storage::open(temp.path(), false, journal).unwrap();
        let id = Identity {
            category: "C04".into(),
            kind: "BAQ".into(),
            scope: herbert_contract::Scope::Global,
            keys: BTreeMap::from([("QueryID".into(), "MyQuery".into())]),
        };
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("Source/Custom.cs", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"return 42;").unwrap();
        zip.start_file(
            "QueryDependencies",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let native = store.preserve_raw(&bytes, "baq").unwrap();
        store
            .journal
            .artifact_acquisitions
            .insert(identity_key(&id), vec![native]);
        let (files, errors) = write_views(&mut store, &id, &json!({"QueryID":"MyQuery"})).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            std::fs::read(temp.path().join("BAQs/MyQuery/MyQuery.baq")).unwrap(),
            bytes
        );
        assert_eq!(
            std::fs::read(temp.path().join("BAQs/MyQuery/Code/Source/Custom.cs")).unwrap(),
            b"return 42;"
        );
        assert!(
            !files
                .iter()
                .any(|f| f.path.contains("index.json") || f.path.ends_with("definition.json"))
        );
        assert_eq!(
            std::fs::read_dir(temp.path().join("BAQs/MyQuery"))
                .unwrap()
                .count(),
            2
        );
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(temp.path().join("Herbert Data"))
                    .unwrap()
                    .file_attributes()
                    & 2,
                0
            );
        }
    }
}
