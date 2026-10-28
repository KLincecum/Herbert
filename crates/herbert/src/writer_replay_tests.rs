//! Replay retained output through the real shared writer. No server is contacted.
use super::*;
use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Instant};

#[test]
#[ignore = "Requires a retained collection and an external qualification directory"]
fn replay_collected_files() {
    let source = std::path::PathBuf::from(std::env::var("HERBERT_REPLAY_SOURCE").unwrap());
    let root = std::env::var("HERBERT_WRITE_BENCH_ROOT").unwrap();
    let manifest: crate::storage::Manifest = serde_json::from_slice(
        &std::fs::read(source.join("Herbert Data/manifest.json")).unwrap()).unwrap();
    let mut expected = BTreeMap::new();
    let mut prepared = Vec::new();
    for artifact in manifest.artifacts.values() {
        let group = artifact.files[0].path.split('/').nth(1).unwrap();
        let files = artifact.files.iter().map(|file| {
            let bytes = std::fs::read(source.join(&file.path)).unwrap();
            assert_eq!(crate::storage::sha256(&bytes), file.sha256);
            assert_eq!(bytes.len() as u64, file.bytes);
            expected.insert(file.path.clone(), (file.sha256.clone(), file.bytes));
            crate::processing::SourceFile {
                name: file.path.rsplit('/').next().unwrap().into(), bytes,
                pointer: String::new(), provenance: None,
            }
        }).collect();
        prepared.push(PreparedRecord { reference_update: None, identity: artifact.identity.clone(),
            outcome: (artifact.acquisition.clone(), artifact.diagnostics.clone()),
            definition: serde_json::json!({"herbertOutputGroup":group}),
            sources: Some((files, vec![])) });
    }
    let dir = tempfile::tempdir_in(root).unwrap();
    let mut journal = crate::storage::new_journal("writer-replay".into());
    journal.company = manifest.company.clone();
    journal.category_names.insert("C13".into(), "Kinetic personalizations".into());
    let shared = Arc::new(Mutex::new(Storage::open(dir.path(), false, journal).unwrap()));
    let pool = crate::file_writers::WriterPool::default();
    let token = tokio_util::sync::CancellationToken::new();
    let start = Instant::now();
    let mut pages = prepared.into_iter();
    let mut write_ms = 0;
    let mut checkpoint_ms = 0;
    loop {
        let page: Vec<_> = pages.by_ref().take(1000).collect();
        if page.is_empty() { break; }
        let at = Instant::now();
        write_shared_page(shared.clone(), page, &pool, &token, |_| {}).unwrap();
        write_ms += at.elapsed().as_millis();
        let at = Instant::now();
        shared.lock().unwrap().checkpoint().unwrap();
        checkpoint_ms += at.elapsed().as_millis();
    }
    let total_ms = start.elapsed().as_millis();
    let storage = shared.lock().unwrap();
    let mut actual = BTreeMap::new();
    for artifact in &storage.journal.artifacts {
        assert!(artifact.diagnostics.is_empty());
        for file in &artifact.files {
            let bytes = std::fs::read(storage.confined(&file.path).unwrap()).unwrap();
            actual.insert(file.path.clone(), (crate::storage::sha256(&bytes), bytes.len() as u64));
        }
    }
    assert_eq!(actual, expected);
    println!("WRITER_REPLAY {}", serde_json::json!({"writeMs":write_ms,
        "checkpointMs":checkpoint_ms,"totalMs":total_ms,"files":actual.len(),
        "bytes":actual.values().map(|(_,n)|n).sum::<u64>(),"allFilesMatch":true}));
    drop(storage); drop(shared); drop(pool);
    dir.close().unwrap();
}
