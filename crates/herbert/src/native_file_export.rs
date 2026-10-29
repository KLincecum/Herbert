//! Native exporters sometimes write a file instead of returning its bytes.
//! Keep its random name and write phase durable. Never delete a guest-chosen path.
use super::*;

const FILE_TRANSFER: &str = "Ice.Lib.FileTransferSvc";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExportFile {
    pub operation_id: String,
    pub identity: Identity,
    pub folder: i32,
    pub server_path: String,
    pub phase: String,
    pub file: Option<FileRef>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub authorization:Option<crate::server_changes::AuthorizationRef>,
}

fn issue(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("NATIVE_EXPORT_FILE_PENDING", message)
}

pub(crate) fn resolve_export_owner(requested: Option<&Identity>, declared: Option<&Identity>, category: &str, company: &str) -> Result<Identity,Diagnostic> {
    if requested.zip(declared).is_some_and(|(a,b)|a!=b) {
        return Err(Diagnostic::new("PLUGIN_IDENTITY_DENIED","Epicor's export cannot replace the requested artifact owner."));
    }
    declared.or(requested).filter(|id|id.category==category && identity_in_scope(id,company)).cloned()
        .ok_or_else(||Diagnostic::new("PLUGIN_IDENTITY_DENIED","Epicor's export requires an artifact owner in the selected category and company."))
}

fn prepare_arguments(
    mut arguments: Value,
    argument: &str,
    path: &str,
) -> Result<Value, Diagnostic> {
    let object = arguments
        .as_object_mut()
        .ok_or_else(|| issue("Epicor's export arguments must be an object."))?;
    if argument.is_empty() || object.contains_key(argument) {
        return Err(issue(
            "The export filename argument must be named and left for Herbert to supply.",
        ));
    }
    object.insert(argument.into(), json!(path));
    Ok(arguments)
}

fn validate_record(record: &ExportFile) -> Result<(), Diagnostic> {
    let id = uuid::Uuid::parse_str(&record.operation_id)
        .map_err(|_| issue("The export ownership ID is invalid."))?;
    let prefix = format!("Herbert-{id}.");
    if id.is_nil()
        || !record.server_path.starts_with(&prefix)
        || storage::safe_relative(&record.server_path)?
            .components()
            .count()
            != 1
    {
        return Err(issue(
            "The server export filename does not match its saved ownership ID.",
        ));
    }
    // SpecialFolder is the native FileTransfer enum, verified from both the
    // live method contract and Epicor.ServiceModel.Utilities.SpecialFolder.
    if !(0..=9).contains(&record.folder) {
        return Err(issue(
            "The folder must be an Epicor FileTransfer SpecialFolder value.",
        ));
    }
    Ok(())
}

async fn save(run: &Arc<Run>, record: ExportFile) -> Result<(), Diagnostic> {
    save_all(run, vec![record]).await
}

async fn save_all(run: &Arc<Run>, records: Vec<ExportFile>) -> Result<(), Diagnostic> {
    collection::update(run, move |storage| {
        for record in records {
            if let Some(existing) = storage.journal.native_export_files.iter_mut().find(|r| {
                r.operation_id == record.operation_id && r.server_path == record.server_path
            }) {
                *existing = record;
            } else {
                storage.journal.native_export_files.push(record);
            }
        }
        Ok(())
    })
    .await
}

async fn phase(run: &Arc<Run>, records: &mut [ExportFile], value: &str) -> Result<(), Diagnostic> {
    for record in records.iter_mut() {
        record.phase = value.into();
    }
    // All companion intents must commit before the single exporter call.
    save_all(run, records.to_vec()).await
}

async fn exists(
    connection: &Connection,
    record: &ExportFile,
    cancel: &CancellationToken,
) -> Result<bool, Diagnostic> {
    let response = connection
        .native_read(
            FILE_TRANSFER,
            "FileExists",
            json!({"folder":record.folder,"serverPath":record.server_path}),
            false,
            cancel,
        )
        .await?;
    response
        .value
        .get("returnObj")
        .and_then(Value::as_bool)
        .ok_or_else(|| issue("Epicor file presence was not returned as a boolean."))
}

async fn cleanup(
    host:&Host,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    record: &mut ExportFile,
) -> Result<(), Diagnostic> {
    validate_record(record)?;
    let owner = collection::snapshot(run).await?;
    if owner.company != connection.company
        || url::Url::parse(&owner.endpoint).ok().as_ref() != Some(&connection.endpoint)
        || connection.caller().await.as_deref() != Some(owner.caller.as_str())
    {
        return Err(issue(
            "The cleanup connection does not match the saved export owner.",
        ));
    }
    match record.phase.as_str() {
        "removed" | "collision" => return Ok(()),
        "reserved" => {
            record.phase = "removed".into();
            return save(run, record.clone()).await;
        }
        "exporting" | "uncertain" => {
            return Err(issue(
                "Herbert cannot tell whether an export on the Epicor server finished. It kept its record of the export so the server file can still be cleaned up. Do not start the same export again by hand; run cleanup from Herbert.",
            )
            .with_context("serverFile", record.server_path.as_str()));
        }
        "exported" | "retained" | "deleting" => {}
        _ => {
            return Err(issue(
                "The saved export phase is unknown; no file was deleted.",
            ));
        }
    }
    let authority=record.authorization.as_ref().ok_or_else(||Diagnostic::new("SERVER_CHANGE_CLEANUP_AUTHORIZATION_REQUIRED","This older export has no recorded cleanup authorization. Preserve it and explicitly authorize its owned recovery scope."))?;
    host.require_owned_cleanup(run,connection,authority).await?;
    // Cancellation stops collection, not cleanup of files already owned by it.
    let cancel = CancellationToken::new();
    if exists(connection, record, &cancel).await? {
        record.phase = "deleting".into();
        save(run, record.clone()).await?;
        let deletion = connection
            .native_read(
                FILE_TRANSFER,
                "FileDelete",
                json!({"folder":record.folder,"serverPath":record.server_path}),
                true,
                &cancel,
            )
            .await;
        // A failed HTTP response may still have deleted the file. Reconcile
        // exact presence before deciding whether another cleanup is needed.
        if exists(connection, record, &cancel).await? {
            return Err(deletion
                .err()
                .unwrap_or_else(|| issue("The owned Epicor's export file remains after deletion.")));
        }
    }
    record.phase = "removed".into();
    save(run, record.clone()).await
}

impl Host {
    pub(super) async fn export_native_file(
        &self,
        plugin: &Plugin,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        identity: Option<&Identity>,
        service: &str,
        method: &str,
        arguments: Value,
        filename_argument: &str,
        folder: i32,
        extension: &str,
        companions: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<FileRef>, Diagnostic> {
        if !plugin.methods.contains(&(service.into(), method.into())) {
            return Err(Diagnostic::new(
                "PLUGIN_CAPABILITY_DENIED",
                "The plugin does not declare this Epicor's export.",
            ));
        }
        let identity = identity
            .filter(|i| {
                i.category == plugin.category.id && identity_in_scope(i, &connection.company)
            })
            .ok_or_else(|| issue("An Epicor file export requires the accepted artifact identity."))?
            .clone();
        let binding=super::server_change_operations::semantic_binding(&plugin.category.id,&plugin.hash,&plugin.category.settings);
        perform_exports(
            self,
            &binding,
            run,
            connection,
            identity,
            service,
            method,
            arguments,
            filename_argument,
            folder,
            extension,
            companions,
            cancel,
        )
        .await
    }
}

#[cfg(test)]
async fn perform_export(
    host:&Host,
    binding:&str,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    identity: Identity,
    service: &str,
    method: &str,
    arguments: Value,
    filename_argument: &str,
    folder: i32,
    extension: &str,
    cancel: &CancellationToken,
) -> Result<FileRef, Diagnostic> {
    Ok(perform_exports(
        host,
        binding,
        run,
        connection,
        identity,
        service,
        method,
        arguments,
        filename_argument,
        folder,
        extension,
        &[],
        cancel,
    )
    .await?
    .remove(0))
}

// Some exporters place a companion file inside their primary ZIP as well.
// Reuse those exact bytes, then clean up both server files normally.
fn embedded_companion(archive: Option<&[u8]>, name: &str) -> Result<Option<Vec<u8>>, Diagnostic> {
    use std::io::Read;
    let Some(bytes) = archive else {
        return Ok(None);
    };
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Ok(None);
    };
    let matches = zip.file_names().filter(|entry| *entry == name).count();
    if matches == 0 {
        return Ok(None);
    }
    if matches != 1 {
        return Err(issue("The export ZIP repeats a decompiler add-on filename."));
    }
    let mut member = zip
        .by_name(name)
        .map_err(|_| issue("The embedded decompiler add-on cannot be opened."))?;
    if member.is_dir() {
        return Err(issue("The embedded decompiler add-on is a directory."));
    }
    let mut bytes = Vec::new();
    member.read_to_end(&mut bytes).map_err(|_| {
        issue("The embedded decompiler add-on failed decompression or checksum validation.")
    })?;
    Ok(Some(bytes))
}

async fn perform_exports(
    host:&Host,
    binding:&str,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    identity: Identity,
    service: &str,
    method: &str,
    arguments: Value,
    filename_argument: &str,
    folder: i32,
    extension: &str,
    companions: &[String],
    cancel: &CancellationToken,
) -> Result<Vec<FileRef>, Diagnostic> {
    let expected=json!({"kind":"nativeExportFiles","service":service,"method":method,"folder":folder});
    let effect_key=format!("nativeFile:{service}/{method}");
    let authority=host.require_server_change(run,connection,&identity.category,&effect_key,Some(binding),&expected,Some(&identity)).await?;
    let journal = collection::snapshot(run).await?;
    if journal.company != connection.company
        || url::Url::parse(&journal.endpoint).ok().as_ref() != Some(&connection.endpoint)
        || connection.caller().await.as_deref() != Some(journal.caller.as_str())
    {
        return Err(issue(
            "The connection does not match the saved export owner.",
        ));
    }
    let mut pending_error = None;
    for mut pending in journal
        .native_export_files
        .into_iter()
        .filter(|r| r.identity == identity && r.phase != "removed" && r.phase != "collision")
    {
        if let Err(error) = cleanup(host, run, connection, &mut pending).await {
            pending_error.get_or_insert(error);
        }
    }
    if let Some(error) = pending_error {
        return Err(error);
    }
    if cancel.is_cancelled() {
        return Err(Diagnostic::new(
            "CANCELED",
            "The export was canceled before it started.",
        ));
    }
    let extensions: Vec<&str> = std::iter::once(extension)
        .chain(companions.iter().map(String::as_str))
        .collect();
    let mut names = std::collections::BTreeSet::new();
    for suffix in &extensions {
        if suffix.is_empty()
            || storage::safe_relative(&format!("export.{suffix}"))?
                .components()
                .count()
                != 1
            || !names.insert(storage::collision_key(&format!("export.{suffix}")))
        {
            return Err(issue(
                "Export extensions must identify distinct portable filenames.",
            ));
        }
    }
    let operation_id = uuid::Uuid::new_v4().to_string();
    let mut records: Vec<ExportFile> = extensions
        .iter()
        .map(|suffix| ExportFile {
            server_path: format!("Herbert-{operation_id}.{suffix}"),
            operation_id: operation_id.clone(),
            identity: identity.clone(),
            folder,
            phase: "reserved".into(),
            file: None,
            authorization:Some(authority.reference()),
        })
        .collect();
    for record in &records {
        validate_record(record)?;
    }
    let arguments = prepare_arguments(arguments, filename_argument, &records[0].server_path)?;
    save_all(run, records.clone()).await?;
    for record in &records {
        if exists(connection, record, cancel).await? {
            phase(run, &mut records, "collision").await?;
            return Err(issue(
                "A generated export filename already exists. No export or deletion was performed.",
            ));
        }
    }
    if cancel.is_cancelled() {
        return Err(Diagnostic::new(
            "CANCELED",
            "The export was canceled before it started.",
        ));
    }
    phase(run, &mut records, "exporting").await?;
    if let Err(error)=host.require_server_change(run,connection,&identity.category,&effect_key,Some(binding),&expected,Some(&identity)).await {
        // No native write was initiated. Do not preserve a false uncertain-export intent.
        phase(run,&mut records,"reserved").await?;
        return Err(error);
    }
    // Wait for the single native write even if collection is canceled.
    // Transport disables replay. An uncertain result retains every file intent.
    if let Err(error) = connection
        .native_read(service, method, arguments, true, &CancellationToken::new())
        .await
    {
        phase(run, &mut records, "uncertain").await?;
        return Err(error);
    }
    phase(run, &mut records, "exported").await?;
    let mut files = Vec::new();
    let mut source_zip: Option<Vec<u8>> = None;
    let mut failure = None;
    for (record, suffix) in records.iter_mut().zip(extensions) {
        let result = async {
            if cancel.is_cancelled() {
                return Err(Diagnostic::new(
                    "CANCELED",
                    "The download was canceled; its server files will be removed.",
                ));
            }
            let bytes = if let Some(bytes) =
                embedded_companion(source_zip.as_deref(), &record.server_path)?
            {
                bytes
            } else {
                let response = connection
                    .native_read(
                        FILE_TRANSFER,
                        "DownloadFile",
                        json!({"folder":folder,"serverPath":record.server_path}),
                        true,
                        cancel,
                    )
                    .await?;
                crate::native_export::decode_bytes(
                    response
                        .value
                        .get("returnObj")
                        .ok_or_else(|| issue("The Epicor file download omitted its bytes."))?,
                )?
            };
            let storage = run.storage.clone();
            let identity = record.identity.clone();
            let suffix = suffix.to_owned();
            let zip_primary = files.is_empty() && suffix == "zip";
            let (file, bytes) = blocking(move || {
                let file = storage
                    .lock()
                    .map_err(|_| issue("Run storage is unavailable."))?
                    .preserve_for(Some(&identity), &bytes, &suffix)?;
                Ok((file, bytes))
            })
            .await?;
            if zip_primary {
                source_zip = Some(bytes);
            }
            record.file = Some(file.clone());
            record.phase = "retained".into();
            save(run, record.clone()).await?;
            Ok(file)
        }
        .await;
        match result {
            Ok(file) => files.push(file),
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    // One failed read or deletion must not prevent cleanup of its companions.
    for record in &mut records {
        if let Err(error) = cleanup(host, run, connection, record).await {
            failure.get_or_insert(error);
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(files)
}

impl Host {
    pub(super) async fn cleanup_native_export_files(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Vec<Diagnostic> {
        let journal = match collection::snapshot(run).await {
            Ok(j) => j,
            Err(e) => return vec![e],
        };
        let mut errors = Vec::new();
        for mut record in journal.native_export_files {
            if let Err(error) = cleanup(self, run, connection, &mut record).await {
                errors.push(error);
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_cannot_supply_or_redirect_cleanup_filename() {
        assert!(
            prepare_arguments(json!({"fileName":"existing.xml"}), "fileName", "owned.xml").is_err()
        );
        let id = uuid::Uuid::new_v4().to_string();
        let mut record = ExportFile {
            operation_id: id.clone(),
            identity: Identity {
                category: "test".into(),
                kind: "test".into(),
                scope: Scope::Global,
                keys: BTreeMap::new(),
            },
            folder: 4,
            server_path: format!("Herbert-{id}.xml"),
            phase: "exported".into(),
            file: None,
            authorization:None,
        };
        assert!(validate_record(&record).is_ok());
        record.server_path = format!("Herbert-{id}.xml/../other.xml");
        assert!(validate_record(&record).is_err());
        record.server_path = "someone-elses-file.xml".into();
        assert!(validate_record(&record).is_err());
    }
}

#[cfg(test)]
#[path = "native_file_export_tests.rs"]
mod lifecycle_tests;

#[cfg(test)]
mod page_owner_tests {
 use super::*;
 #[test]
 fn page_export_owner_is_required_scoped_and_cannot_replace_exact_owner() {
  let id=Identity {category:"configurators".into(),kind:"Configurator".into(),scope:Scope::Company {company:"C".into()},keys:BTreeMap::from([("Company".into(),"C".into()),("ConfigID".into(),"A".into())])};
  assert_eq!(resolve_export_owner(None,Some(&id),"configurators","C").unwrap(),id);
  assert_eq!(resolve_export_owner(Some(&id),None,"configurators","C").unwrap(),id);
  assert!(resolve_export_owner(None,None,"configurators","C").is_err());
  assert!(resolve_export_owner(None,Some(&id),"report-behavior","C").is_err());
  assert!(resolve_export_owner(None,Some(&id),"configurators","Other").is_err());
  let mut other=id.clone();other.keys.insert("ConfigID".into(),"B".into());
  assert!(resolve_export_owner(Some(&id),Some(&other),"configurators","C").is_err());
 }
}
