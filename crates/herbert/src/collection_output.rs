//! Complete page definitions go straight to named JSON, without a per-item cache
//! or a second acquisition/processing pass.
use crate::storage::{Storage, identity_key};
use herbert_contract::{Artifact, Completeness, Diagnostic, Identity};
use serde_json::Value;

#[path = "shared_page_writer.rs"]
mod shared_page_writer;
pub(crate) use shared_page_writer::write_shared_page;

#[cfg(test)]
#[path = "collection_output_batch_tests.rs"]
mod batch_tests;

/// Inline content is prepared before the page writer acquires the run mutex.
#[derive(Clone)]
pub(crate) struct ReferenceUpdate { records: Vec<Value>, core_sha256: String }
pub(crate) struct PreparedRecord {
    pub(crate) reference_update: Option<ReferenceUpdate>,
    identity: Identity,
    outcome: (Completeness, Vec<Diagnostic>),
    definition: Value,
    sources: Option<(Vec<crate::processing::SourceFile>, Vec<Diagnostic>)>,
}

impl PreparedRecord {
    /// Preserve final selected-definition diagnostics through the prepared writer.
    /// A producer may attach only the exact record owner.
    pub(crate) fn extend_diagnostics(&mut self, mut diagnostics: Vec<Diagnostic>) -> Result<(), Diagnostic> {
        if diagnostics.iter().any(|diagnostic| diagnostic.identity.as_ref().is_some_and(|owner| owner != &self.identity)) {
            return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "A final definition diagnostic refers to another artifact owner."));
        }
        for diagnostic in &mut diagnostics { diagnostic.identity = Some(self.identity.clone()); }
        for diagnostic in diagnostics { if !self.outcome.1.contains(&diagnostic) { self.outcome.1.push(diagnostic); } }
        Ok(())
    }
    pub(crate) fn set_reference_processing(&mut self, records: Vec<Value>, diagnostics: Vec<Diagnostic>, complete: bool) {
        if !complete { self.outcome.1.push(Diagnostic::new("PROCESSOR_FAILED", "A local definition processor did not complete. Its original diagnostic is retained; independent sources and valid current references remain available.")); }
        self.outcome.1.extend(diagnostics);
        let core_sha256=if let Some(source)=self.sources.as_ref().and_then(|(sources,_)|sources.first()){crate::storage::sha256(&source.bytes)}else{crate::storage::sha256(&serde_json::to_vec_pretty(&self.definition).expect("A JSON value can be serialized"))};
        self.reference_update=Some(ReferenceUpdate{records,core_sha256});
    }
}

pub(crate) fn prepare_page(records: Vec<(Identity, Value)>) -> Result<Vec<PreparedRecord>, Diagnostic> {
    records.into_iter().map(|(identity, definition)| {
        let outcome = collection_outcome(&definition)?;
        let sources = if definition["herbertDirectOutput"] == true {
            Some(crate::processing::prepare_direct_sources(&identity, &definition)?)
        } else { None };
        // Inline source bytes are already prepared. Keep only filesystem references
        // and grouping metadata for the direct writer, not another copy of the page.
        let definition = if sources.is_some() { serde_json::json!({
            "herbertOutputGroup":definition.get("herbertOutputGroup"),
            "herbertSources":{"nativeFiles":definition.pointer("/herbertSources/nativeFiles").cloned().unwrap_or_else(|| serde_json::json!([]))}
        }) } else { definition };
        Ok(PreparedRecord { identity, outcome, definition, sources, reference_update: None })
    }).collect()
}

/// Optional per-record result supplied by collectors that materialize partial
/// definitions. Legacy complete-page collectors retain their existing default.
fn collection_outcome(definition: &Value) -> Result<(Completeness, Vec<Diagnostic>), Diagnostic> {
    let Some(status) = definition.get("herbertCollectionStatus") else {
        return Ok((Completeness::Collected, vec![]));
    };
    let invalid = || Diagnostic::new("OUTPUT_INVALID", "The collected record status is invalid.");
    let completeness: Completeness = serde_json::from_value(status["completeness"].clone()).map_err(|_| invalid())?;
    let diagnostics: Vec<Diagnostic> = serde_json::from_value(status["diagnostics"].clone()).map_err(|_| invalid())?;
    Ok((completeness, diagnostics))
}
fn apply_outcome(artifact: &mut Artifact, outcome: (Completeness, Vec<Diagnostic>)) {
    artifact.acquisition = outcome.0;
    artifact.diagnostics.extend(outcome.1);
    if artifact.acquisition != Completeness::Collected || artifact.diagnostics.iter().any(|d| (d.code.starts_with("REFERENCE_") || d.code.starts_with("PROCESSOR_")) && d.code != "REFERENCE_SCAN_COVERAGE") { artifact.processing = "partial".into(); }
    crate::processing_status::reconcile(artifact);
}

fn artifact(storage: &Storage, identity: Identity, files: Vec<herbert_contract::FileRef>, diagnostics: Vec<Diagnostic>) -> Artifact {
    Artifact { identity, classification: if storage.journal.scope == "fullBackup" {
        "discoveredDefinition" } else { "explicitlyRequestedDefinition" }.into(),
        acquisition: Completeness::Collected, integrity: "hostHashComputed".into(),
        processing: "derived".into(), files, dependencies: vec![], diagnostics,
        source_consistency: "nonAtomicUnknown".into() }
}
fn commit_artifact(storage: &mut Storage, artifact: Artifact) {
    storage.manifest.artifacts.insert(identity_key(&artifact.identity), artifact.clone());
    storage.journal.artifacts.retain(|old| old.identity != artifact.identity);
    storage.journal.artifacts.push(artifact);
}

pub(crate) fn write_page(
    storage: &mut Storage,
    records: Vec<(Identity, Value)>,
) -> Result<(), Diagnostic> {
    write_page_with_progress(storage, records, |_| {})
}

pub(crate) fn write_page_with_progress(
    storage: &mut Storage,
    records: Vec<(Identity, Value)>,
    mut progress: impl FnMut(usize),
) -> Result<(), Diagnostic> {
    let mut written = 0;
    for (identity, definition) in records {
        write_record(storage, identity, definition)?;
        written += 1;
        progress(written);
    }
    // Save run status once per page, not once per type or duplicate representation.
    storage.checkpoint()
}

/// Write one complete artifact. The caller can release the run lock between
/// artifacts so native fetches and status reads do not wait for the whole page.
pub(crate) fn write_record(
    storage: &mut Storage,
    identity: Identity,
    definition: Value,
) -> Result<(), Diagnostic> {
    let outcome = collection_outcome(&definition)?;
    let (files, diagnostics) = if definition["herbertDirectOutput"] == true {
        // The page checkpoint owns these receipts. Each write computes a hash.
        // Recovery verifies saved output before reuse. If the process stops before the checkpoint,
        // an unrecorded file is preserved as an unknown file on the next run.
        storage.page_owns_derived_files = true;
        let result = crate::processing::write_views(storage, &identity, &definition);
        storage.page_owns_derived_files = false;
        result?
    } else {
        let bytes = serde_json::to_vec_pretty(&definition).map_err(|_| {
            Diagnostic::new(
                "OUTPUT_INVALID",
                "Cannot serialize the collected definition.",
            )
        })?;
        let name = format!("{}.json", crate::output_layout::artifact_name(&identity));
        let (file, conflict) = storage.write_derived(&identity, &name, &bytes)?;
        let diagnostics = if conflict {
            vec![Diagnostic::new(
                "USER_EDIT_CONFLICT",
                "Your edited JSON was preserved; the current server version is in Conflicts.",
            )]
        } else {
            vec![]
        };
        (vec![file], diagnostics)
    };
    let mut artifact = artifact(storage, identity, files, diagnostics);
    apply_outcome(&mut artifact, outcome);
    commit_artifact(storage, artifact);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::Scope;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// Repeatable local I/O benchmark. Run explicitly on the benchmark disk.
    #[test]
    #[ignore]
    fn measure_personalization_file_writes() {
        let base = std::env::var("HERBERT_WRITE_BENCH_ROOT").expect("Set the benchmark output root");
        let temp = tempfile::tempdir_in(base).unwrap();
        let mut storage = Storage::open(temp.path(), false,
            crate::storage::new_journal("write-measurement".into())).unwrap();
        let start = std::time::Instant::now();
        let mut naming = std::time::Duration::ZERO;
        let mut writing = std::time::Duration::ZERO;
        for index in 0..1000 {
            let id = Identity { category: "kinetic-personalizations".into(), kind: "Favorite".into(),
                scope: Scope::Personal { company: "C".into(), owner: format!("user{}", index % 100) },
                keys: BTreeMap::from([("ItemSeq".into(), index.to_string())]) };
            storage.page_owns_derived_files = true;
            let at = std::time::Instant::now();
            storage.group_artifact(&id, &format!("User - {}", index % 100)).unwrap();
            naming += at.elapsed();
            let at = std::time::Instant::now();
            let definition = json!({"herbertDirectOutput":true,"value":"source".repeat(1024),
                "herbertSources":{"files":[{"name":"Favorite.xml","text":"<Favorite/>"}]}});
            write_record(&mut storage, id, definition).unwrap();
            writing += at.elapsed();
        }
        let at = std::time::Instant::now();
        storage.checkpoint().unwrap();
        println!("WRITE_BENCH naming_ms={} writing_ms={} checkpoint_ms={} total_ms={} files={}",
            naming.as_millis(), writing.as_millis(), at.elapsed().as_millis(), start.elapsed().as_millis(),
            storage.journal.artifacts.iter().map(|a| a.files.len()).sum::<usize>());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn faster_path_checks_still_reject_an_outside_junction() {
        // Keep the native junction fixture in the external build workspace.
        let executable = std::env::current_exe().unwrap();
        let workspace = executable.parent().unwrap();
        let temp = tempfile::tempdir_in(workspace).unwrap();
        let outside = tempfile::tempdir_in(workspace).unwrap();
        std::fs::write(outside.path().join("marker"), b"outside").unwrap();
        let storage = Storage::open(temp.path(), false,
            crate::storage::new_journal("junction-check".into())).unwrap();
        // Create a real mount-point reparse record without a shell. This
        // exercises junction handling even when symlink privilege is absent.
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn DeviceIoControl(handle: *mut std::ffi::c_void, code: u32,
                input: *const u8, input_len: u32, output: *mut u8,
                output_len: u32, returned: *mut u32,
                overlapped: *mut std::ffi::c_void) -> i32;
        }
        let link = storage.root().join("redirect");
        std::fs::create_dir(&link).unwrap();
        let target = std::fs::canonicalize(outside.path()).unwrap();
        let display = target.to_string_lossy();
        let plain = display.trim_start_matches(r"\\?\");
        let substitute: Vec<u16> = format!(r"\??\{plain}").encode_utf16().collect();
        let print: Vec<u16> = plain.encode_utf16().collect();
        let substitute_bytes = (substitute.len() * 2) as u16;
        let print_bytes = (print.len() * 2) as u16;
        let data_len = 8 + substitute_bytes + 2 + print_bytes + 2;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&0xA0000003u32.to_le_bytes());
        for value in [data_len, 0, 0, substitute_bytes, substitute_bytes + 2, print_bytes] {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        for value in substitute.into_iter().chain([0]).chain(print).chain([0]) {
            buffer.extend_from_slice(&value.to_le_bytes());
        }
        let directory = std::fs::OpenOptions::new().access_mode(0x40000000)
            .share_mode(7).custom_flags(0x02200000).open(&link).unwrap();
        let mut returned = 0;
        // SAFETY: the handle stays open, the input buffer is complete, and
        // this synchronous control call has no output or overlapped buffer.
        let result = unsafe { DeviceIoControl(directory.as_raw_handle(), 0x000900A4,
            buffer.as_ptr(), buffer.len() as u32, std::ptr::null_mut(), 0,
            &mut returned, std::ptr::null_mut()) };
        assert_ne!(result, 0, "Cannot create junction: {}", std::io::Error::last_os_error());
        drop(directory);
        assert!(storage.confined("redirect/marker").is_err());
        std::fs::remove_dir(&link).unwrap();
        assert_eq!(std::fs::read(outside.path().join("marker")).unwrap(), b"outside");
    }

    #[test]
    fn page_receipts_survive_reopen_and_preserve_user_edits() {
        let temp = tempfile::tempdir().unwrap();
        let journal = crate::storage::new_journal("page-receipts".into());
        let id = Identity { category: "kinetic-personalizations".into(), kind: "Favorite".into(),
            scope: Scope::Personal { company: "C".into(), owner: "a/b".into() },
            keys: BTreeMap::from([("FolderSeq".into(), "1".into())]) };
        let definition = json!({"herbertDirectOutput":true,"herbertOutputGroup":"User - a/b",
            "value":"first","herbertSources":{"files":[{"name":"Favorite.xml","text":"<Favorite/>"}]}});
        let mut storage = Storage::open(temp.path(), false, journal.clone()).unwrap();
        write_page(&mut storage, vec![(id.clone(), definition.clone())]).unwrap();
        assert!(!temp.path().join("Herbert Data/derived").exists());
        let file = storage.manifest.artifacts[&identity_key(&id)].files[0].clone();
        assert!(file.path.contains("/User - a%2Fb/"));
        drop(storage);
        let mut storage = Storage::open(temp.path(), false, journal.clone()).unwrap();
        write_page(&mut storage, vec![(id.clone(), definition.clone())]).unwrap();
        assert!(storage.manifest.artifacts[&identity_key(&id)].diagnostics.is_empty());
        std::fs::write(storage.confined(&file.path).unwrap(), b"user edit").unwrap();
        write_page(&mut storage, vec![(id.clone(), definition)]).unwrap();
        assert_eq!(std::fs::read(storage.confined(&file.path).unwrap()).unwrap(), b"user edit");
        assert!(storage.manifest.artifacts[&identity_key(&id)].diagnostics.iter()
            .any(|d| d.code == "USER_EDIT_CONFLICT"));
    }

    #[test]
    fn page_names_survive_reopen_and_resolve_case_collisions() {
        let temp=tempfile::tempdir().unwrap();
        let journal=crate::storage::new_journal("page-names".into());
        let id=|name: &str| Identity { category:"baqs-and-ubaqs".into(),kind:"BAQ".into(),scope:Scope::Global,
            keys:BTreeMap::from([("QueryID".into(),name.into())]) };
        let mut store=Storage::open(temp.path(),false,journal.clone()).unwrap();
        let definition=json!({"herbertDirectOutput":true,"value":"original"});
        write_page(&mut store,vec![(id("Example"),definition.clone()),(id("example"),definition.clone())]).unwrap();
        let first=store.manifest.artifacts[&identity_key(&id("Example"))].files[0].path.clone();
        let second=store.manifest.artifacts[&identity_key(&id("example"))].files[0].path.clone();
        assert_ne!(first.to_lowercase(),second.to_lowercase());
        assert!(!temp.path().join("Herbert Data/names/identities").exists());
        drop(store);
        let mut store=Storage::open(temp.path(),false,journal).unwrap();
        write_page(&mut store,vec![(id("example"),definition.clone()),(id("Example"),definition)]).unwrap();
        assert_eq!(store.manifest.artifacts[&identity_key(&id("Example"))].files[0].path,first);
        assert_eq!(store.manifest.artifacts[&identity_key(&id("example"))].files[0].path,second);
    }

    #[test]
    fn interrupted_page_does_not_claim_uncommitted_files() {
        let temp = tempfile::tempdir().unwrap();
        let journal = crate::storage::new_journal("interrupted-page".into());
        let id = Identity { category: "kinetic-personalizations".into(), kind: "Favorite".into(),
            scope: Scope::Global, keys: BTreeMap::from([("ID".into(), "1".into())]) };
        let definition = json!({"herbertDirectOutput":true,"value":"first"});
        let mut storage = Storage::open(temp.path(), false, journal.clone()).unwrap();
        write_record(&mut storage, id.clone(), definition.clone()).unwrap();
        let file = storage.manifest.artifacts[&identity_key(&id)].files[0].clone();
        drop(storage);
        let mut storage = Storage::open(temp.path(), false, journal).unwrap();
        std::fs::write(storage.confined(&file.path).unwrap(), b"uncommitted edit").unwrap();
        write_page(&mut storage, vec![(id.clone(), definition)]).unwrap();
        assert_eq!(std::fs::read(storage.confined(&file.path).unwrap()).unwrap(), b"uncommitted edit");
        assert!(storage.manifest.artifacts[&identity_key(&id)].diagnostics.iter()
            .any(|d| d.code == "USER_EDIT_CONFLICT"));
    }

    #[test]
    fn page_writes_named_json_without_inventory_or_processing_copies() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = crate::storage::new_journal("direct-page".into());
        journal.company = "C".into();
        journal
            .category_names
            .insert("ud-codes".into(), "UD codes".into());
        let mut storage = Storage::open(temp.path(), false, journal).unwrap();
        let id = Identity {
            category: "ud-codes".into(),
            kind: "UDCodeType".into(),
            scope: Scope::Company {
                company: "C".into(),
            },
            keys: BTreeMap::from([("CodeTypeID".into(), "Example".into())]),
        };
        let data = json!({"UDCodeType":[{"CodeTypeID":"Example"}],
            "UDCodes":[{"CodeID":"A","CodeDesc":"Unicode Ω"}]});
        let path = temp.path().join("UD codes/Example/Example.json");
        let mut progress = Vec::new();
        write_page_with_progress(&mut storage, vec![(id.clone(), data.clone())], |count| {
            assert!(
                path.is_file(),
                "Progress must follow the actual file write."
            );
            progress.push(count);
        })
        .unwrap();
        assert_eq!(progress, vec![1]);
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
            data
        );
        assert!(storage.journal.inventory_data.is_empty());
        assert!(storage.journal.definition_views.is_empty());
        assert!(storage.journal.acquisitions.is_empty());
        assert_eq!(storage.journal.artifacts.len(), 1);
        assert_eq!(storage.journal.artifacts[0].files.len(), 1);
        std::fs::write(&path, b"user edit").unwrap();
        write_page(&mut storage, vec![(id, data)]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"user edit");
        assert!(
            storage.journal.artifacts[0].files[0]
                .path
                .starts_with("Conflicts/")
        );
    }
}

#[cfg(test)]
#[path = "writer_replay_tests.rs"]
mod writer_replay_tests;
