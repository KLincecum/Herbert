//! Local fixtures exercise the same prepared page path used by the host.
use super::*;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

fn records(count: usize) -> Vec<(Identity, Value)> {
    (0..count).map(|index| (Identity { category: "C13".into(), kind: "Favorite".into(),
        scope: herbert_contract::Scope::Personal { company: "C".into(), owner: format!("user{}", index % 100) },
        keys: BTreeMap::from([("ItemSeq".into(), index.to_string())]) },
        json!({"herbertDirectOutput":true,"herbertOutputGroup":format!("User - {}",index % 100),
            "value":"source".repeat(1024),"herbertSources":{"files":[{"name":"Favorite.xml","text":"<Favorite/>"}]}})))
        .collect()
}

#[test]
fn writer_snapshot_keeps_selected_receipts_without_copying_the_backup() {
    let root = tempfile::tempdir().unwrap();
    let mut storage = Storage::open(root.path(), false, crate::storage::new_journal("writer-scope".into())).unwrap();
    let data = records(2);
    let owner = data[0].0.clone();
    write_page(&mut storage, data).unwrap();
    storage.journal.references = vec![json!({"unrelated":"large accumulated reference index"})];
    let receipt = storage.manifest.artifacts[&crate::storage::identity_key(&owner)].clone();
    storage.journal.artifact_acquisitions.insert(crate::storage::identity_key(&owner), receipt.files.clone());
    let view = storage.page_output_snapshot([&owner]);
    assert!(view.journal.references.is_empty());
    assert!(view.journal.artifacts.is_empty());
    assert!(view.manifest.references.is_empty());
    assert_eq!(view.manifest.artifacts.len(), 1);
    assert_eq!(view.manifest.artifacts[&crate::storage::identity_key(&owner)], receipt);
    assert_eq!(view.journal.artifact_acquisitions[&crate::storage::identity_key(&owner)], receipt.files);
    assert_eq!(view.artifact_folder(&owner).unwrap(), storage.artifact_folder(&owner).unwrap());
    for file in &receipt.files { view.verify(file).unwrap(); }
}

/// Comparison experiment only. The installed writer and its defaults are not changed.
/// A queue entry contains 100 FILES, not 100 records or 100 writer threads.
#[test]
#[ignore = "Compare two and four workers taking chunks of 100 prepared files"]
fn measure_hundred_file_chunks() {
    use std::{collections::VecDeque, time::Instant};
    let root = std::env::var("HERBERT_WRITE_BENCH_ROOT").expect("Select the qualification disk");
    let mut reference = None;
    // Alternate order across three trials. Every run gets a fresh destination.
    for (trial, workers) in [(1, 2), (1, 4), (2, 4), (2, 2), (3, 2), (3, 4)] {
        let dir = tempfile::tempdir_in(&root).unwrap();
        let mut storage = Storage::open(
            dir.path(),
            false,
            crate::storage::new_journal("chunks".into()),
        )
        .unwrap();
        storage.page_owns_derived_files = true;
        let started = Instant::now();
        let records = prepare_page(records(1000)).unwrap();
        let mut jobs = Vec::new();
        let mut expected = BTreeMap::new();
        for record in records {
            storage
                .group_artifact(
                    &record.identity,
                    record.definition["herbertOutputGroup"].as_str().unwrap(),
                )
                .unwrap();
            let folder = storage.artifact_folder(&record.identity).unwrap();
            let (sources, diagnostics) = record.sources.unwrap();
            assert!(diagnostics.is_empty());
            for source in sources {
                let path = format!("{folder}/{}", source.name);
                assert!(
                    expected
                        .insert(
                            path,
                            (
                                crate::storage::sha256(&source.bytes),
                                source.bytes.len() as u64
                            )
                        )
                        .is_none()
                );
                jobs.push((record.identity.clone(), source));
            }
        }
        assert_eq!(jobs.len(), 2000);
        let mut chunks = VecDeque::new();
        let mut jobs = jobs.into_iter();
        loop {
            let chunk = jobs.by_ref().take(100).collect::<Vec<_>>();
            if chunk.is_empty() {
                break;
            }
            assert_eq!(chunk.len(), 100);
            chunks.push_back(chunk);
        }
        assert_eq!(chunks.len(), 20);
        let prepare_ms = started.elapsed().as_millis();
        let queue = Mutex::new(chunks);
        let write_started = Instant::now();
        let results = std::thread::scope(|scope| {
            let handles = (0..workers)
                .map(|_| {
                    let queue = &queue;
                    let storage = &storage;
                    scope.spawn(move || {
                        let mut written = Vec::new();
                        loop {
                            let Some(chunk) = queue.lock().unwrap().pop_front() else {
                                break;
                            };
                            // One assignment, then 100 ordinary file writes with no
                            // shared work-queue access between those files.
                            for (identity, source) in chunk {
                                let (file, conflict) = storage
                                    .write_derived(&identity, &source.name, &source.bytes)
                                    .unwrap();
                                assert!(!conflict);
                                written.push((identity, file));
                            }
                        }
                        written
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let write_ms = write_started.elapsed().as_millis();
        let worker_files = results.iter().map(Vec::len).collect::<Vec<_>>();
        assert!(worker_files.iter().all(|count| count % 100 == 0));
        let commit_started = Instant::now();
        let mut artifacts = BTreeMap::new();
        for (identity, file) in results.into_iter().flatten() {
            let entry = artifacts
                .entry(identity_key(&identity))
                .or_insert_with(|| artifact(&storage, identity, vec![], vec![]));
            entry.files.push(file);
        }
        for (_, artifact) in artifacts {
            commit_artifact(&mut storage, artifact);
        }
        storage.checkpoint().unwrap();
        let commit_ms = commit_started.elapsed().as_millis();
        let total_ms = started.elapsed().as_millis();
        // Independently read the final files, not just the writer's receipts.
        let mut actual = BTreeMap::new();
        for file in storage.journal.artifacts.iter().flat_map(|a| &a.files) {
            let bytes = std::fs::read(storage.confined(&file.path).unwrap()).unwrap();
            actual.insert(
                file.path.clone(),
                (crate::storage::sha256(&bytes), bytes.len() as u64),
            );
        }
        assert_eq!(actual, expected);
        if let Some(reference) = &reference {
            assert_eq!(&actual, reference);
        } else {
            reference = Some(actual.clone());
        }
        println!(
            "CHUNK_BENCH {}",
            json!({"trial":trial,"writers":workers,"filesPerChunk":100,
            "chunks":20,"files":actual.len(),"bytes":actual.values().map(|(_,n)|n).sum::<u64>(),
            "prepareMs":prepare_ms,"writeMs":write_ms,"commitMs":commit_ms,"totalMs":total_ms,
            "workerFiles":worker_files,"allFilesMatch":true})
        );
        // Drop the destination lock before tempfile removes this exact fixture.
        drop(storage);
        dir.close().unwrap();
    }
}

#[test]
fn prepared_batches_match_serial_files_release_run_mutex_and_preserve_edits() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut serial = Storage::open(
        a.path(),
        false,
        crate::storage::new_journal("serial".into()),
    )
    .unwrap();
    write_page(&mut serial, records(220)).unwrap();
    let shared = Arc::new(Mutex::new(
        Storage::open(b.path(), false, crate::storage::new_journal("batch".into())).unwrap(),
    ));
    let token = tokio_util::sync::CancellationToken::new();
    write_shared_page(
        shared.clone(),
        prepare_page(records(220)).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &token,
        |_| {
            // Another writer can briefly publish its receipt. This callback must
            // still acquire the run lock while the rest of the page is in flight.
            drop(shared.lock().unwrap());
        },
    )
    .unwrap();
    let batch = Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();
    batch.checkpoint().unwrap();
    let files = |s: &Storage| {
        s.journal
            .artifacts
            .iter()
            .flat_map(|a| a.files.iter())
            .map(|f| (f.path.clone(), (f.sha256.clone(), f.bytes)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(files(&serial), files(&batch));
    let edited = batch.journal.artifacts[0].files[0].path.clone();
    let path = batch.confined(&edited).unwrap();
    std::fs::write(&path, b"user edit").unwrap();
    let journal = batch.journal.clone();
    drop(batch);
    let shared = Arc::new(Mutex::new(Storage::open(b.path(), false, journal).unwrap()));
    write_shared_page(
        shared.clone(),
        prepare_page(records(220)).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &token,
        |_| {},
    )
    .unwrap();
    let batch = shared.lock().unwrap();
    batch.checkpoint().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"user edit");
    assert_eq!(
        batch
            .journal
            .artifacts
            .iter()
            .flat_map(|a| a.diagnostics.iter())
            .filter(|d| d.code == "USER_EDIT_CONFLICT")
            .count(),
        1
    );
}

#[test]
fn canceled_prepared_page_does_not_commit_unwritten_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let shared = Arc::new(Mutex::new(
        Storage::open(
            dir.path(),
            false,
            crate::storage::new_journal("cancel".into()),
        )
        .unwrap(),
    ));
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    assert!(
        write_shared_page(
            shared.clone(),
            prepare_page(records(220)).unwrap(),
            &crate::file_writers::WriterPool::default(),
            &token,
            |_| {}
        )
        .is_err()
    );
    assert!(shared.lock().unwrap().journal.artifacts.is_empty());
}

#[test]
fn shared_pool_writes_mixed_pages_from_multiple_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Arc::new(Mutex::new(
        Storage::open(
            directory.path(),
            false,
            crate::storage::new_journal("mixed".into()),
        )
        .unwrap(),
    ));
    let pool = crate::file_writers::WriterPool::default();
    std::thread::scope(|scope| {
        for category in ["C01", "C04", "C08", "C13"] {
            let storage = storage.clone();
            let pool = &pool;
            scope.spawn(move || {
                let mut page = records(60);
                for (index, (identity, definition)) in page.iter_mut().enumerate() {
                    identity.category = category.into();
                    if index % 2 == 0 {
                        *definition = json!({"plain": index});
                    }
                }
                write_shared_page(
                    storage,
                    prepare_page(page).unwrap(),
                    pool,
                    &tokio_util::sync::CancellationToken::new(),
                    |_| {},
                )
                .unwrap();
            });
        }
    });
    drop(pool);
    let storage = storage.lock().unwrap();
    assert_eq!(storage.journal.artifacts.len(), 240);
    assert_eq!(
        storage
            .journal
            .artifacts
            .iter()
            .map(|a| a.files.len())
            .sum::<usize>(),
        360
    );
    for artifact in &storage.journal.artifacts {
        for file in &artifact.files {
            storage.verify(file).unwrap();
        }
    }
}

#[test]
fn failure_in_a_chunk_does_not_strand_later_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Mutex::new(
        Storage::open(
            dir.path(),
            false,
            crate::storage::new_journal("failed-chunk".into()),
        )
        .unwrap(),
    ));
    let mut page = records(1);
    page[0].1["herbertSources"]["files"] = Value::Array(
        (0..240)
            .map(|i| json!({"name":format!("part-{i}.txt"),"text":"part"}))
            .collect(),
    );
    // A missing acquired file fails verification in the first chunk. Later
    // chunks still drain. Preserve their verified receipts as a partial artifact.
    let file = herbert_contract::FileRef {
        role: "native".into(),
        path: "missing.zip".into(),
        bytes: 1,
        sha256: "00".repeat(32),
    };
    page[0].1["herbertSources"]["nativeFiles"] = json!([{"name":"native.zip","file":file}]);
    assert!(
        write_shared_page(
            storage.clone(),
            prepare_page(page.clone()).unwrap(),
            &crate::file_writers::WriterPool::default(),
            &tokio_util::sync::CancellationToken::new(),
            |_| {}
        )
        .is_err()
    );
    let journal = {
        let mut saved = storage.lock().unwrap();
        assert_eq!(saved.journal.artifacts.len(), 1);
        let partial = &saved.journal.artifacts[0];
        assert_eq!(partial.acquisition, Completeness::Partial);
        assert_eq!(partial.files.len(), 241);
        for file in &partial.files {
            saved.verify(file).unwrap();
        }
        let available = saved.preserve_raw(b"native fixture", "zip").unwrap();
        page[0].1["herbertSources"]["nativeFiles"][0]["file"] = json!(available);
        saved.checkpoint().unwrap();
        saved.journal.clone()
    };
    drop(storage);
    let recovered = Arc::new(Mutex::new(
        Storage::open(dir.path(), false, journal).unwrap(),
    ));
    write_shared_page(
        recovered.clone(),
        prepare_page(page).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let saved = recovered.lock().unwrap();
    let artifact = &saved.journal.artifacts[0];
    assert_eq!(artifact.acquisition, Completeness::Collected);
    assert_eq!(artifact.files.len(), 242);
    assert!(artifact.diagnostics.is_empty());
    for file in &artifact.files {
        assert!(!file.path.starts_with("Conflicts/"));
        saved.verify(file).unwrap();
    }
}

#[test]
fn shared_native_copy_matches_serial_output() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let mut serial = Storage::open(
        a.path(),
        false,
        crate::storage::new_journal("native-serial".into()),
    )
    .unwrap();
    let mut batch = Storage::open(
        b.path(),
        false,
        crate::storage::new_journal("native-shared".into()),
    )
    .unwrap();
    let original = serial.preserve_raw(b"native package bytes", "bpm").unwrap();
    let other = batch.preserve_raw(b"native package bytes", "bpm").unwrap();
    assert_eq!(original.sha256, other.sha256);
    let mut page = records(1);
    page[0].1["herbertSources"]["nativeFiles"] = json!([{"name":"original.bpm","file":original}]);
    serial.journal.artifact_acquisitions.insert(identity_key(&page[0].0),vec![original.clone()]);
    batch.journal.artifact_acquisitions.insert(identity_key(&page[0].0),vec![other.clone()]);
    write_page(&mut serial, page.clone()).unwrap();
    page[0].1["herbertSources"]["nativeFiles"][0]["file"] = json!(other);
    let shared = Arc::new(Mutex::new(batch));
    write_shared_page(
        shared.clone(),
        prepare_page(page).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &tokio_util::sync::CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let shared = shared.lock().unwrap();
    let files = |storage: &Storage| {
        storage
            .journal
            .artifacts
            .iter()
            .flat_map(|a| &a.files)
            .map(|f| (f.path.clone(), (f.sha256.clone(), f.bytes, f.role.clone())))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(files(&serial), files(&shared));
    for file in &shared.journal.artifacts[0].files {
        shared.verify(file).unwrap();
    }
}

#[test]
fn recovery_replays_missing_outputs_and_preserves_changed_files() {
    let directory = tempfile::tempdir().unwrap();
    let page = records(3);
    let mut storage = Storage::open(directory.path(), false,
        crate::storage::new_journal("reconcile-output".into())).unwrap();
    write_page(&mut storage, page.clone()).unwrap();
    storage.journal.cursors.insert("C13".into(), "finished-page".into());
    storage.journal.discovery_finished.insert("C13".into(), true);
    let missing = storage.journal.artifacts[0].files[0].clone();
    let changed = storage.journal.artifacts[1].files[0].clone();
    storage.checkpoint().unwrap();
    let journal = storage.journal.clone();
    let missing_path = storage.confined(&missing.path).unwrap();
    let changed_path = storage.confined(&changed.path).unwrap();
    drop(storage);
    std::fs::remove_file(&missing_path).unwrap();
    std::fs::write(&changed_path, b"keep my edits").unwrap();
    let mut storage = Storage::open(directory.path(), false, journal).unwrap();
    storage.reconcile_downloads().unwrap();
    assert!(!storage.journal.cursors.contains_key("C13"));
    assert!(!storage.journal.discovery_finished.contains_key("C13"));
    assert_eq!(storage.journal.artifacts.iter()
        .filter(|a| a.acquisition == Completeness::Partial).count(), 2);
    let shared = Arc::new(Mutex::new(storage));
    write_shared_page(shared.clone(), prepare_page(page).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &tokio_util::sync::CancellationToken::new(), |_| {}).unwrap();
    let storage = shared.lock().unwrap();
    storage.verify(&missing).unwrap();
    assert_eq!(std::fs::read(changed_path).unwrap(), b"keep my edits");
    let conflicts = storage.journal.artifacts.iter().flat_map(|a| &a.files)
        .filter(|f| f.role == "derivedConflict").collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].sha256, changed.sha256);
    for file in storage.journal.artifacts.iter().flat_map(|a| &a.files) {
        storage.verify(file).unwrap();
    }
}

#[test]
fn renamed_files_without_a_page_checkpoint_are_reused_after_readback() {
    let directory = tempfile::tempdir().unwrap();
    let journal = crate::storage::new_journal("uncommitted-output".into());
    let shared = Arc::new(Mutex::new(Storage::open(directory.path(), false, journal.clone()).unwrap()));
    let pool = crate::file_writers::WriterPool::default();
    let token = tokio_util::sync::CancellationToken::new();
    write_shared_page(shared.clone(), prepare_page(records(4)).unwrap(), &pool, &token, |_| {}).unwrap();
    let expected = shared.lock().unwrap().journal.artifacts.iter().flat_map(|a| &a.files)
        .map(|f| (f.path.clone(), f.sha256.clone())).collect::<BTreeMap<_,_>>();
    // Reopen the last durable journal, without the in-memory page receipts.
    drop(shared);
    let shared = Arc::new(Mutex::new(Storage::open(directory.path(), false, journal).unwrap()));
    write_shared_page(shared.clone(), prepare_page(records(4)).unwrap(), &pool, &token, |_| {}).unwrap();
    let storage = shared.lock().unwrap();
    assert!(storage.journal.artifacts.iter().all(|a| a.diagnostics.is_empty()));
    let actual = storage.journal.artifacts.iter().flat_map(|a| &a.files)
        .map(|f| { storage.verify(f).unwrap(); (f.path.clone(), f.sha256.clone()) })
        .collect::<BTreeMap<_,_>>();
    assert_eq!(actual, expected);
}

#[test]
fn abrupt_writer_child() {
    let Some(root) = std::env::var_os("HERBERT_ABRUPT_WRITER_FIXTURE") else { return; };
    let shared = Arc::new(Mutex::new(Storage::open(std::path::Path::new(&root), false,
        crate::storage::new_journal("abrupt-writer".into())).unwrap()));
    write_shared_page(shared, prepare_page(records(4)).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &tokio_util::sync::CancellationToken::new(), |_| {}).unwrap();
    // Stop without a page checkpoint or normal storage cleanup.
    std::process::exit(0);
}

#[test]
fn fresh_process_recovery_reuses_uncheckpointed_files() {
    let directory = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("collection_output::batch_tests::abrupt_writer_child")
        .arg("--exact").env("HERBERT_ABRUPT_WRITER_FIXTURE", directory.path())
        .status().unwrap();
    assert!(status.success());
    let shared = Arc::new(Mutex::new(Storage::open(directory.path(), false,
        crate::storage::new_journal("abrupt-writer".into())).unwrap()));
    write_shared_page(shared.clone(), prepare_page(records(4)).unwrap(),
        &crate::file_writers::WriterPool::default(),
        &tokio_util::sync::CancellationToken::new(), |_| {}).unwrap();
    let storage = shared.lock().unwrap();
    assert_eq!(storage.journal.artifacts.len(), 4);
    assert!(storage.journal.artifacts.iter().all(|a| a.diagnostics.is_empty()));
    for file in storage.journal.artifacts.iter().flat_map(|a| &a.files) {
        assert!(!file.path.starts_with("Conflicts/"));
        storage.verify(file).unwrap();
    }
}

#[test]
fn interrupted_stream_keeps_original_and_removes_temporary_file() {
    struct Broken(bool);
    impl std::io::Read for Broken {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.0 { return Err(std::io::Error::other("fixture read failure")); }
            self.0 = true;
            out[..4].copy_from_slice(b"part");
            Ok(4)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(directory.path(), false,
        crate::storage::new_journal("interrupted-stream".into())).unwrap();
    let identity = records(1).remove(0).0;
    let (original, _) = storage.write_derived(&identity, "view.txt", b"complete original").unwrap();
    let path = storage.confined(&original.path).unwrap();
    let entries = || std::fs::read_dir(path.parent().unwrap()).unwrap().count();
    let before = entries();
    assert!(storage.write_derived_stream(&identity, "view.txt", &mut Broken(false)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"complete original");
    assert_eq!(entries(), before);
}

#[test]
fn collected_record_status_survives_serial_shared_and_reopen() {
    for parallel in [false,true] {
        let dir=tempfile::tempdir().unwrap();
        let mut data=records(2);
        data[0].1["herbertCollectionStatus"]=json!({"completeness":Completeness::Partial,"diagnostics":[Diagnostic::new("SOURCE_UNVERIFIED","Native source was unavailable.")]});
        let mut storage=Storage::open(dir.path(),false,crate::storage::new_journal("status".into())).unwrap();
        if parallel {
            let shared=Arc::new(Mutex::new(storage));
            write_shared_page(shared.clone(),prepare_page(data).unwrap(),&crate::file_writers::WriterPool::default(),&tokio_util::sync::CancellationToken::new(),|_|{}).unwrap();
            storage=Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();
        } else {write_page(&mut storage,data).unwrap();}
        storage.checkpoint().unwrap();
        let journal=storage.journal.clone();drop(storage);
        let storage=Storage::open(dir.path(),false,journal).unwrap();
        let partial=storage.journal.artifacts.iter().find(|a|a.identity.keys["ItemSeq"]=="0").unwrap();
        assert_eq!(partial.acquisition,Completeness::Partial);
        assert_eq!(partial.processing,"partial");
        assert!(partial.diagnostics.iter().any(|d|d.code=="SOURCE_UNVERIFIED"));
        assert!(!partial.files.is_empty());for file in &partial.files {storage.verify(file).unwrap();}
        assert_eq!(storage.journal.artifacts.iter().find(|a|a.identity.keys["ItemSeq"]=="1").unwrap().acquisition,Completeness::Collected);
        drop(storage);dir.close().unwrap();
    }
    let mut invalid=records(1);invalid[0].1["herbertCollectionStatus"]=json!({"completeness":"invented","diagnostics":[]});
    assert!(prepare_page(invalid).is_err());
}


#[test]
fn final_selected_definition_diagnostics_survive_shared_writer_without_changing_files_or_acquisition(){
    let root=tempfile::tempdir().unwrap();
    let shared=Arc::new(Mutex::new(Storage::open(root.path(),false,crate::storage::new_journal("selected-processing-status".into())).unwrap()));
    let pool=crate::file_writers::WriterPool::default();let cancel=tokio_util::sync::CancellationToken::new();let data=records(1);let owner=data[0].0.clone();
    write_shared_page(shared.clone(),prepare_page(data.clone()).unwrap(),&pool,&cancel,|_|{}).unwrap();let baseline=shared.lock().unwrap().journal.artifacts[0].clone();
    let mut failure=Diagnostic::new("OPTIONAL_CAPTURE_FAILED","The requested optional capture failed.");failure.context.insert("processingIncomplete".into(),json!(true));
    let ordinary=Diagnostic::new("NATIVE_OBSERVATION","Preserve this ordinary producer diagnostic too.");
    let mut prepared=prepare_page(data.clone()).unwrap();prepared[0].extend_diagnostics(vec![ordinary.clone(),failure.clone()]).unwrap();
    write_shared_page(shared.clone(),prepared,&pool,&cancel,|_|{}).unwrap();
    {let storage=shared.lock().unwrap();let a=&storage.journal.artifacts[0];assert_eq!(a.acquisition,Completeness::Collected);assert_eq!(a.files,baseline.files);assert_eq!(a.processing,"partial");assert_eq!(a.diagnostics.len(),2);assert!(a.diagnostics.iter().all(|d|d.identity.as_ref()==Some(&owner)));assert!(a.diagnostics.iter().any(|d|d.code==ordinary.code));for file in &a.files{storage.verify(file).unwrap();}}
    // Fresh producer records replace obsolete failures; they do not inherit markers.
    write_shared_page(shared.clone(),prepare_page(data.clone()).unwrap(),&pool,&cancel,|_|{}).unwrap();
    {let storage=shared.lock().unwrap();let a=&storage.journal.artifacts[0];assert_eq!(a.acquisition,Completeness::Collected);assert_eq!(a.files,baseline.files);assert_eq!(a.processing,"derived");assert!(a.diagnostics.is_empty());}
    let mut foreign=failure;let mut other=owner.clone();other.category="other-category".into();foreign.identity=Some(other);let mut prepared=prepare_page(data).unwrap();assert!(prepared[0].extend_diagnostics(vec![foreign]).is_err());assert!(shared.lock().unwrap().journal.artifacts[0].diagnostics.is_empty());
}
