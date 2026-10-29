//! Real collection dispatch, with an inert local producer and the compiled Configuration references.
use super::*;

#[tokio::test]
#[ignore = "Explicit compiled Configuration references and inert malformed-processor fixture; no ERP"]
async fn configuration_references_reference_invalid_processor_with_coverage_is_nonauthoritative() {
    let root = tempfile::tempdir().unwrap();
    let wasm = PathBuf::from(std::env::var("HERBERT_CONFIGURATION_REFERENCES_WASM").unwrap());
    let real = Plugin::load(&wasm).await.unwrap();
    let descriptor = GuestReply::Descriptor {
        abi: PLUGIN_ABI,
        category: real.category.clone(),
        methods: vec![],
        entities: vec![],
        binary_exports: vec![],
        response_policies: vec![],
    };
    let source = Identity {
        category: "fixture".into(),
        kind: "definition".into(),
        scope: Scope::Company {
            company: "C".into(),
        },
        keys: BTreeMap::from([("ID".into(), "one".into())]),
    };
    let invalid = json!({"identity":{"category":"configuration-references","kind":"DatabaseTableReference","scope":source.scope,"keys":{"Schema":"Erp","Table":"Part"}},"occurrences":[{"source":source,"pointer":"/absent"}]});
    let reply = GuestReply::References {
        records: vec![
            json!({"identity":{"category":"configuration-references","kind":"DatabaseTableReference","scope":source.scope,"keys":{"Schema":"Erp","Table":"Part"}},"occurrences":[{"source":source,"pointer":""}]}),
            invalid,
        ],
        diagnostics: vec![Diagnostic::new(
            "REFERENCE_SCAN_COVERAGE",
            "Reported coverage does not validate an invented pointer.",
        )],
    };
    let bad = scripted(root.path(), descriptor, reply.clone(), reply).await;
    let host = Host::with_roots(
        root.path().join("state"),
        root.path().join("plugins"),
        Some(root.path().join("coordination")),
    );
    let mut journal = storage::new_journal("invalid-reference".into());
    journal.company = "C".into();
    let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
    *run.collection_context.lock().await = Some(CollectionContext {
        processors: vec![bad],
        page_size: None,
        full: true,
        processed_inputs: Arc::new(Mutex::new(BTreeSet::new())),
    });
    let records = host
        .prepare_reference_page(
            &run,
            vec![(source, json!({"DBSchemaName":"Erp","DBTableName":"Part"}))],
            &run.cancel,
        )
        .await
        .unwrap();
    crate::collection_output::write_shared_page(
        run.storage.clone(),
        records,
        &host.file_writers,
        &run.cancel,
        |_| {},
    )
    .unwrap();
    let context = {
        let s = run.storage.lock().unwrap();
        let artifact = &s.journal.artifacts[0];
        assert_eq!(artifact.processing, "partial");
        assert_eq!(artifact.acquisition, Completeness::Collected);
        assert!(
            artifact
                .diagnostics
                .iter()
                .any(|d| d.code == "PROCESSOR_REFERENCE_INVALID")
        );
        let context =
            super::super::super::reference_processing::reference_context(&s.journal, "configuration-references");
        assert_eq!(context["producerCount"], 1);
        assert_eq!(context["producerReferenceErrors"], true);
        assert_eq!(s.journal.references.len(), 1);
        assert_eq!(artifact.dependencies.len(), 1);
        context
    };
    let request = GuestRequest {
        abi: PLUGIN_ABI,
        operation: "discover".into(),
        category: "configuration-references".into(),
        company: "C".into(),
        identity: None,
        cursor: None,
        page_size: None,
        state: context,
        response: None,
        error: None,
    };
    let GuestReply::InventoryData { inventory, .. } = real.call(request, run.cancel.clone()).await.unwrap() else { panic!("Reference discovery must return its records in bulk") };
    assert!(matches!(
        *inventory,
        GuestReply::Inventory {
            authoritative: false,
            ..
        }
    ));
}

async fn producer(root: &Path, processor: &Plugin) -> Plugin {
    let mut category = processor.category.clone();
    category.id = "ud-codes".into();
    category.name = "Reference fixture".into();
    category.roles = vec!["collector".into(), "pagedDefinitions".into()];
    category.helpers.clear();
    category.artifacts.clear();
    let descriptor = GuestReply::Descriptor {
        abi: PLUGIN_ABI,
        category,
        methods: vec![],
        entities: vec![],
        binary_exports: vec![],
        response_policies: vec![],
    };
    let id = Identity {
        category: "ud-codes".into(),
        kind: "Fixture".into(),
        scope: Scope::Company {
            company: "C".into(),
        },
        keys: BTreeMap::from([("ID".into(), "Owned".into())]),
    };
    let definition =
        json!({"herbertDirectOutput":true,"DBSchemaName":"Erp","DBTableName":"Company"});
    let acquisition = GuestReply::Definition {
        identity: id.clone(),
        format: "fixture".into(),
        definition: definition.clone(),
        completeness: Completeness::Collected,
        diagnostics: vec![],
    };
    let inventory = GuestReply::InventoryData {
        records: vec![(id.clone(), definition)],
        inventory: Box::new(GuestReply::Inventory {
            identities: vec![id],
            next_cursor: None,
            authoritative: true,
            diagnostics: vec![],
        }),
    };
    scripted(root, descriptor, acquisition, inventory).await
}

async fn scripted(
    root: &Path,
    descriptor: GuestReply,
    acquisition: GuestReply,
    inventory: GuestReply,
) -> Plugin {
    let mut data = String::new();
    let mut offset = 4096u32;
    let mut literal = |bytes: &[u8]| {
        let start = offset;
        let escaped = bytes
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        data += &format!("(data (i32.const {start}) \"{escaped}\")\n");
        offset += bytes.len() as u32 + 1;
        (start, bytes.len() as u32)
    };
    let descriptor_marker = literal(b"descriptor");
    let discover_marker = literal(b"discover");
    let packed = |(p, n): (u32, u32)| ((p as u64) << 32) | n as u64;
    let desc = packed(literal(&serde_json::to_vec(&descriptor).unwrap()));
    let acquire = packed(literal(&serde_json::to_vec(&acquisition).unwrap()));
    let discover = packed(literal(&serde_json::to_vec(&inventory).unwrap()));
    let has = |(p, n)| format!("(call $has (local.get $len) (i32.const {p}) (i32.const {n}))");
    let wat = format!(
        r#"(module (memory (export "memory") 2) {data}
      (func (export "hoover_abi") (result i32) i32.const 1)
      (func (export "hoover_alloc") (param i32) (result i32) i32.const 0)
      (func (export "hoover_free") (param i32 i32))
      (func $has (param $len i32) (param $needle i32) (param $n i32) (result i32)
        (local $i i32)(local $j i32)
        (block $absent(loop $outer
          (br_if $absent(i32.gt_u(i32.add(local.get $i)(local.get $n))(local.get $len)))
          (local.set $j(i32.const 0))
          (block $different(loop $inner
            (if(i32.eq(local.get $j)(local.get $n))(then(return(i32.const 1))))
            (br_if $different(i32.ne(i32.load8_u(i32.add(local.get $i)(local.get $j)))(i32.load8_u(i32.add(local.get $needle)(local.get $j)))))
            (local.set $j(i32.add(local.get $j)(i32.const 1)))(br $inner)))
          (local.set $i(i32.add(local.get $i)(i32.const 1)))(br $outer))) i32.const 0)
      (func(export "hoover_call")(param i32)(param $len i32)(result i64)
        (if(result i64){is_descriptor}(then(i64.const {desc}))(else
          (if(result i64){is_discover}(then(i64.const {discover}))(else(i64.const {acquire})))))))"#,
        is_descriptor = has(descriptor_marker),
        is_discover = has(discover_marker)
    );
    let path = root.join("producer.wasm");
    std::fs::write(&path, wat::parse_str(wat).unwrap()).unwrap();
    Plugin::load(&path).await.unwrap()
}

#[tokio::test]
#[ignore = "Explicit compiled Configuration references; loopback admission only, no ERP"]
async fn configuration_references_reference_real_exact_acquire_and_full_ordering() {
    let wasm = PathBuf::from(std::env::var("HERBERT_CONFIGURATION_REFERENCES_WASM").unwrap());
    for exact in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let plugins = root.path().join("plugins");
        std::fs::create_dir(&plugins).unwrap();
        std::fs::copy(&wasm, plugins.join("configuration-references.wasm")).unwrap();
        let processor = Plugin::load(&plugins.join("configuration-references.wasm")).await.unwrap();
        let producer = producer(&plugins, &processor).await;
        let host = Host::with_roots(
            root.path().join("state"),
            plugins,
            Some(root.path().join("coordination")),
        );
        let (connection, _server) = connection_mode(true).await;
        let mut journal = storage::new_journal("reference-dispatch".into());
        journal.company = "C".into();
        journal.endpoint = connection.endpoint.to_string();
        journal.caller = "fixture".into();
        journal.temporary_access_enabled = false;
        let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
        if exact {
            host.collect_one(
                run.clone(),
                connection.clone(),
                producer,
                vec![processor],
                Identity {
                    category: "ud-codes".into(),
                    kind: "Fixture".into(),
                    scope: Scope::Company {
                        company: "C".into(),
                    },
                    keys: BTreeMap::from([("ID".into(), "Owned".into())]),
                },
                None,
                false,
                None,
            )
            .await
            .unwrap();
        } else {
            host.clone()
                .collect(
                    run.clone(),
                    connection.clone(),
                    vec!["configuration-references".into(), "ud-codes".into()],
                    vec![],
                    true,
                    None,
                )
                .await;
        }
        let s = run.storage.lock().unwrap();
        assert_eq!(s.journal.references.len(), 1, "{:?}", s.journal.diagnostics);
        let source = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity.category == "ud-codes")
            .unwrap();
        assert_eq!(source.dependencies.len(), 1);
        assert_eq!(source.processing, "derived");
        let input = &s.journal.definition_views[&storage::identity_key(&source.identity)];
        assert_eq!(input.role, "referenceInput");
        s.verify(input).unwrap();
        if !exact {
            assert!(
                s.journal
                    .artifacts
                    .iter()
                    .any(|a| a.identity.category == "configuration-references"
                        && a.acquisition == Completeness::Collected)
            );
            assert_eq!(s.journal.phase, "completed", "{:?}", s.journal.diagnostics);
        }
        let summary = crate::progress_snapshot::summary(&s.journal);
        let source = summary["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["identity"]["category"] == "ud-codes")
            .unwrap();
        assert!(source["diagnostics"].as_array().unwrap().is_empty());
        assert!(!source["referenceCoverage"].as_array().unwrap().is_empty());
    }
}

#[tokio::test]
#[ignore = "Compiled single-plugin host pipeline with saved local index; no ERP connection"]
async fn reference_saved_index_host_pipeline() {
    let saved: storage::RunJournal = serde_json::from_slice(&std::fs::read(std::env::var_os("HERBERT_PROFILE_JOURNAL").unwrap()).unwrap()).unwrap();
    let plugin = Plugin::load(&PathBuf::from(std::env::var_os("HERBERT_CONFIGURATION_REFERENCES_WASM").unwrap())).await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let host = Host::with_roots(root.path().join("state"),root.path().join("plugins"),Some(root.path().join("coordination")));
    // Only local caller admission is simulated. Unexpected native operations fail.
    let (connection, _server) = connection_company(false, saved.company.clone()).await;
    let mut journal=storage::new_journal("reference-host-profile".into());
    journal.company=saved.company.clone(); journal.endpoint=connection.endpoint.to_string(); journal.caller="fixture".into();
    journal.temporary_access_enabled=false; journal.references=saved.references.clone();
    journal.categories=vec!["configuration-references".into()];journal.category_names=saved.category_names.clone();
    let run=run(Storage::open(&root.path().join("output"),false,journal).unwrap());
    *run.collection_context.lock().await=Some(CollectionContext {processors:vec![plugin.clone()],page_size:None,full:true,processed_inputs:Arc::new(Mutex::new(BTreeSet::new()))});
    let start=std::time::Instant::now();
    let targets=host.discover_targets(&run,&connection,&plugin,&[plugin.clone()],None).await.unwrap();
    host.acquire_targets(&run,&connection,&plugin,&[plugin.clone()],targets,None,true).await.unwrap();
    let elapsed=start.elapsed().as_millis();
    let s=run.storage.lock().unwrap();
    assert_eq!(s.journal.artifacts.len(),saved.references.len(),"Diagnostic codes: {:?}",s.journal.diagnostics.iter().map(|d| &d.code).collect::<Vec<_>>());
    assert_eq!(run.activity.statistics()["workTimings"]["configuration-references"]["pluginCall"]["calls"],1,"Bulk host collection must not reacquire each reference");
    for artifact in &s.journal.artifacts {
        assert_eq!(artifact.acquisition,Completeness::Collected);
        for file in &artifact.files {s.verify(file).unwrap();}
        let file=&artifact.files[0];
        let actual:Value=serde_json::from_slice(&std::fs::read(s.confined(&file.path).unwrap()).unwrap()).unwrap();
        assert!(saved.references.iter().any(|original| *original==actual),"The host must preserve complete reference provenance");
    }
    println!("single-plugin host profile: references={} elapsed_ms={} guest_calls=1 all_outputs_verified=true",s.journal.artifacts.len(),elapsed);
}

