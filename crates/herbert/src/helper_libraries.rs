//! Own temporary native library instances. Each operation gets its own library
//! identity, so teardown never depends on another operation's library lifetime.
use crate::{connection::Connection, helpers, storage};
use fs2::FileExt;
use herbert_contract::{Diagnostic, HelperContract};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LibraryLease {
    pub schema_version: u32,
    pub holder: String,
    pub endpoint: String,
    pub company: String,
    pub caller: String,
    pub template_id: String,
    pub plugin_hash: String,
    pub instance: HelperContract,
    pub owner_marker: String,
    pub phase: String,
    #[serde(default)]
    pub transitions: Vec<String>,
    /// Read old journals without making a code digest an authorization gate.
    #[serde(default, skip_serializing)]
    pub package_sha256: Option<String>,
    #[serde(default)]
    pub frozen_package: Option<String>,
    /// The run authority the instance was installed under. Cleanup uses it after
    /// revocation; leases from earlier versions have none (own-journal match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<crate::server_changes::AuthorizationRef>,
}

/// A one-shot operation keeps this lock until teardown finishes. Another host
/// must not remove a library while its original operation is using it.
pub(crate) struct OperationGuard {
    _file: File,
}
impl OperationGuard {
    pub(crate) fn acquire(root: &Path, holder: &str) -> Result<Self, Diagnostic> {
        let id = uuid::Uuid::parse_str(holder).map_err(|_| state_error())?;
        if id.to_string() != holder {
            return Err(state_error());
        }
        let directory = root.join("helper-operations");
        reject_link(&directory)?;
        fs::create_dir_all(&directory).map_err(|_| state_error())?;
        let path = directory.join(format!("{holder}.lock"));
        reject_link(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| state_error())?;
        file.try_lock_exclusive().map_err(|_| {
            Diagnostic::new(
                "HELPER_OPERATION_ACTIVE",
                "The original helper operation is still active in another host.",
            )
        })?;
        Ok(Self { _file: file })
    }
}

pub(crate) struct LeaseFile {
    path: PathBuf,
    pub lease: LibraryLease,
}

impl LeaseFile {
    /// Read one already-owned generation. Recovery must not reserve a replacement
    /// when the ownership file is missing or corrupt.
    pub(crate) fn existing(
        root: &Path,
        holder: &str,
        template_id: &str,
    ) -> Result<Self, Diagnostic> {
        let directory = root.join("helper-libraries");
        reject_link(&directory)?;
        let key = storage::sha256(
            &serde_json::to_vec(&(holder, template_id)).map_err(|_| state_error())?,
        );
        let path = directory.join(format!("{key}.json"));
        reject_link(&path)?;
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Diagnostic::new(
                    "HELPER_RECOVERY_LEASE_REQUIRED",
                    "The retained helper ownership file is missing. Preserve the saved run and server staging; do not create a replacement generation.",
                )
            } else {
                state_error()
            }
        })?;
        let lease: LibraryLease = serde_json::from_slice(&bytes).map_err(|_| state_error())?;
        if lease.schema_version != 1 || lease.holder != holder || lease.template_id != template_id {
            return Err(Diagnostic::new(
                "HELPER_RECOVERY_OWNER_MISMATCH",
                "The current helper ownership file does not match the retained operation and template.",
            ));
        }
        Ok(Self { path, lease })
    }

    pub(crate) fn open(
        root: &Path,
        holder: &str,
        claim: &HelperContract,
        plugin_hash: &str,
        connection: &Connection,
        caller: &str,
    ) -> Result<Self, Diagnostic> {
        let directory = root.join("helper-libraries");
        reject_link(&directory)?;
        fs::create_dir_all(&directory).map_err(|_| state_error())?;
        let key = storage::sha256(&serde_json::to_vec(&(holder, &claim.library_id)).unwrap());
        let path = directory.join(format!("{key}.json"));
        reject_link(&path)?;
        let lease = match fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<LibraryLease>(&bytes).map_err(|_| state_error())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let id = uuid::Uuid::new_v4();
                let mut instance = claim.clone();
                // Epicor.Functions.LibraryId enforces 30 characters on the
                // inspected ICE 5.1.100.29 runtime. IsValidLibraryId did not
                // enforce that bound. Keep the full GUID as GlobalID/owner
                // marker. The native name uses a prefix and 29 random hex digits.
                // This is a native identifier constraint, not a workload limit.
                instance.library_id = format!("H{}", &id.simple().to_string()[..29]);
                instance.global_id = id.to_string();
                // Runtime ownership needs exact hashes and signatures, not a
                // duplicate installer package at every journal checkpoint.
                instance.installation = None;
                LibraryLease {
                    schema_version: 1,
                    holder: holder.into(),
                    endpoint: connection.endpoint.to_string(),
                    company: connection.company.clone(),
                    caller: caller.into(),
                    template_id: claim.library_id.clone(),
                    plugin_hash: plugin_hash.into(),
                    instance,
                    owner_marker: format!("Herbert temporary helper; lease {id}"),
                    phase: "reserved".into(),
                    transitions: vec!["reserved".into()],
                    package_sha256: None,
                    frozen_package: None,
                    authorization: None,
                }
            }
            Err(_) => return Err(state_error()),
        };
        if lease.schema_version != 1
            || lease.holder != holder
            || lease.endpoint != connection.endpoint.as_str()
            || lease.company != connection.company
            || lease.caller != caller
            || lease.template_id != claim.library_id
        {
            return Err(Diagnostic::new(
                "HELPER_OWNER_MISMATCH",
                "The saved Epicor library lease belongs to another operation, source or Epicor user.",
            ));
        }
        let file = Self { path, lease };
        file.checkpoint()?;
        Ok(file)
    }

    pub(crate) fn checkpoint(&self) -> Result<(), Diagnostic> {
        storage::atomic_write(
            &self.path,
            &serde_json::to_vec_pretty(&self.lease).map_err(|_| state_error())?,
        )
    }

    /// Archive a confirmed removed generation before reserving its successor.
    /// A crash before replacement leaves the old lease and an identical archive.
    pub(crate) fn rotate_removed(&mut self) -> Result<(), Diagnostic> {
        if self.lease.phase != "removed" {
            return Err(state_error());
        }
        let directory = self.path.parent().ok_or_else(state_error)?.join("history");
        reject_link(&directory)?;
        fs::create_dir_all(&directory).map_err(|_| state_error())?;
        let prior_id =
            uuid::Uuid::parse_str(&self.lease.instance.global_id).map_err(|_| state_error())?;
        let archive = directory.join(format!("{prior_id}.json"));
        reject_link(&archive)?;
        let bytes = serde_json::to_vec_pretty(&self.lease).map_err(|_| state_error())?;
        match fs::read(&archive) {
            Ok(previous) if previous == bytes => {}
            Ok(_) => return Err(state_error()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                storage::atomic_write(&archive, &bytes)?
            }
            Err(_) => return Err(state_error()),
        }
        let id = uuid::Uuid::new_v4();
        self.lease.instance.library_id = format!("H{}", &id.simple().to_string()[..29]);
        self.lease.instance.global_id = id.to_string();
        self.lease.owner_marker = format!("Herbert temporary helper; lease {id}");
        self.lease.phase = "reserved".into();
        self.lease.transitions = vec!["reserved".into()];
        self.lease.package_sha256 = None;
        self.lease.frozen_package = None;
        self.checkpoint()
    }

    pub(crate) fn phase(&mut self, phase: &str) -> Result<(), Diagnostic> {
        self.lease.phase = phase.into();
        self.lease.transitions.push(phase.into());
        self.checkpoint()
    }

    pub(crate) fn freeze_package(&mut self, bytes: &[u8]) -> Result<Vec<u8>, Diagnostic> {
        if self.lease.frozen_package.is_some() {
            return self.frozen_package();
        }
        let name = format!("source-{}.efxj", self.lease.instance.global_id);
        let path = self.path.parent().ok_or_else(state_error)?.join(&name);
        reject_link(&path)?;
        storage::atomic_write(&path, bytes)?;
        self.lease.frozen_package = Some(name);
        self.checkpoint()?;
        Ok(bytes.to_vec())
    }
    pub(crate) fn frozen_package(&self) -> Result<Vec<u8>, Diagnostic> {
        let name=self.lease.frozen_package.as_ref().ok_or_else(||Diagnostic::new("HELPER_RECOVERY_SOURCE_REQUIRED","This helper has no retained operation source. Preserve its staging; recovery cannot substitute the author's currently edited package."))?;
        let path = self
            .path
            .parent()
            .ok_or_else(state_error)?
            .join(storage::safe_relative(name)?);
        reject_link(&path)?;
        fs::read(path).map_err(|_| state_error())
    }

    pub(crate) fn package(&self, template: &Value) -> Result<(Vec<u8>, Value), Diagnostic> {
        if !template.is_object() || !template["Library"].is_object() {
            return Err(state_error());
        }
        let mut package = template.clone();
        package["LibraryId"] = json!(self.lease.instance.library_id);
        let library = package
            .get_mut("Library")
            .filter(|library| library.is_object())
            .ok_or_else(state_error)?;
        library["GlobalID"] = json!(self.lease.instance.global_id);
        library["Owner"] = json!(self.lease.caller);
        library["OwnedByCompany"] = json!(self.lease.company);
        library["SharedWithGroup"] = json!("");
        library["Published"] = json!(false);
        library["Notes"] = json!(self.lease.owner_marker);
        library["LibraryMappings"] = json!([{"Company": self.lease.company, "Allowed": true}]);
        // Check every function and read-only source rule before the first native
        // write. Only native identity/scope metadata is different from the template.
        for function in &self.lease.instance.functions {
            helpers::verify_package(
                &self.lease.instance,
                &function.function_id,
                &package,
                &self.lease.company,
                false,
            )?;
        }
        // Native EFXJ seeds carry a UTF-8 BOM. Preserve that wire format when
        // rebinding metadata. Do not turn the export into a different format.
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(serde_json::to_vec(&package).map_err(|_| state_error())?);
        Ok((bytes, package))
    }

    pub(crate) fn verify_compiled(&self, package: &Value) -> Result<(), Diagnostic> {
        self.verify_owned(package, false)?;
        let library = &package["Library"];
        let functions = library["Functions"].as_array().ok_or_else(state_error)?;
        if library["Disabled"].as_bool() != Some(false)
            || functions.iter().any(|function| {
                function["Disabled"].as_bool() != Some(false)
                    || function["Invalid"].as_bool() != Some(false)
            })
        {
            return Err(Diagnostic::new(
                "HELPER_COMPILATION_UNVERIFIED",
                "Epicor readback did not find enabled, valid functions in the exact owned helper.",
            ));
        }
        Ok(())
    }

    pub(crate) fn verify_owned(&self, package: &Value, published: bool) -> Result<(), Diagnostic> {
        let library = &package["Library"];
        if library["Owner"].as_str() != Some(self.lease.caller.as_str())
            || library["OwnedByCompany"].as_str() != Some(self.lease.company.as_str())
            || library["Notes"].as_str() != Some(self.lease.owner_marker.as_str())
        {
            return Err(Diagnostic::new(
                "HELPER_NOT_OWNED",
                "Epicor library ownership metadata changed. Preserve the library without overwriting or removing it.",
            ));
        }
        for function in &self.lease.instance.functions {
            helpers::verify_package(
                &self.lease.instance,
                &function.function_id,
                package,
                &self.lease.company,
                published,
            )?;
        }
        Ok(())
    }
}

pub(crate) fn for_holder(
    root: &Path,
    holder: &str,
) -> Result<(Vec<LeaseFile>, Vec<Diagnostic>), Diagnostic> {
    let directory = root.join("helper-libraries");
    reject_link(&directory)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], vec![])),
        Err(_) => return Err(state_error()),
    };
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(_) => {
                diagnostics.push(state_error());
                continue;
            }
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        // One unreadable or unrelated lease must not strand other verified owned
        // resources. Preserve the suspect file and report it after valid cleanup.
        let read = || -> Result<Option<LeaseFile>, Diagnostic> {
            reject_link(&path)?;
            let value: Value = serde_json::from_slice(&fs::read(&path).map_err(|_| state_error())?)
                .map_err(|_| state_error())?;
            let owner = value["holder"].as_str().ok_or_else(state_error)?;
            if uuid::Uuid::parse_str(owner)
                .map_err(|_| state_error())?
                .to_string()
                != owner
            {
                return Err(state_error());
            }
            if owner != holder {
                return Ok(None);
            }
            let lease: LibraryLease = serde_json::from_value(value).map_err(|_| state_error())?;
            let key = storage::sha256(&serde_json::to_vec(&(holder, &lease.template_id)).unwrap());
            if path.file_name().and_then(|name| name.to_str())
                != Some(format!("{key}.json").as_str())
            {
                return Err(state_error());
            }
            Ok(Some(LeaseFile {
                path: path.clone(),
                lease,
            }))
        };
        match read() {
            Ok(Some(file)) => files.push(file),
            Ok(None) => {}
            Err(error) => diagnostics.push(
                Diagnostic::new(&error.code, error.message)
                    .with_context("ownershipFile", path.display().to_string()),
            ),
        }
    }
    Ok((files, diagnostics))
}

pub(crate) async fn export(
    connection: &Connection,
    library: &str,
    cancel: &CancellationToken,
) -> Result<Value, Diagnostic> {
    let native = connection.native_read(
        "Ice.Lib.EfxLibraryDesignerSvc",
        "ExportLibrary",
        json!({"libraryID": library, "options": {
            "Mode": 0, "Format": 1, "Package": "", "PackageVersion": "", "Publisher": "", "InstallAsHidden": false
        }}),
        true,
        cancel,
    ).await?;
    crate::native_artifact_backend::library_backup(&native.value, library).map(|(_, package)| package)
}

pub(crate) async fn exists(
    connection: &Connection,
    library: &str,
    cancel: &CancellationToken,
) -> Result<bool, Diagnostic> {
    let native = connection
        .native_read(
            "Ice.Lib.EfxLibraryDesignerSvc",
            "GetLibrary",
            json!({"libraryId": library}),
            false,
            cancel,
        )
        .await?;
    let rows = native
        .value
        .pointer("/returnObj/EfxLibrary")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Diagnostic::new(
                "HELPER_READBACK_INVALID",
                "The Epicor library read omitted its identity table.",
            )
        })?;
    if rows.is_empty() {
        return Ok(false);
    }
    if rows.len() != 1 || rows[0]["LibraryID"].as_str() != Some(library) {
        return Err(Diagnostic::new(
            "HELPER_READBACK_INVALID",
            "The Epicor read did not identify one exact helper library.",
        ));
    }
    Ok(true)
}

fn reject_link(path: &Path) -> Result<(), Diagnostic> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(Diagnostic::new(
            "UNSAFE_PATH",
            "Epicor helper ownership files cannot use symbolic links or junctions.",
        ));
    }
    Ok(())
}

fn state_error() -> Diagnostic {
    Diagnostic::new(
        "HELPER_OWNERSHIP_STATE_FAILED",
        "Cannot read or saved progress Epicor helper ownership. Preserve the state without a change in Epicor.",
    )
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn removed_fixture(root: &Path) -> LeaseFile {
        let holder = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4();
        let lease = LibraryLease {
            schema_version: 1,
            holder: holder.clone(),
            endpoint: "https://fixture.invalid/".into(),
            company: "FIXTURE".into(),
            caller: "fixture-caller".into(),
            template_id: "FixtureTemplate".into(),
            plugin_hash: "fixture-plugin-hash".into(),
            instance: HelperContract {
                purpose: Default::default(),
                coverage: Default::default(),
                library_id: format!("H{}", &id.simple().to_string()[..29]),
                global_id: id.to_string(),
                company_argument: "iCompany".into(),
                functions: vec![],
                installation: None,
            },
            owner_marker: format!("Herbert temporary helper; lease {id}"),
            phase: "removed".into(),
            transitions: vec!["reserved".into(), "ready".into(), "removed".into()],
            package_sha256: Some("fixture-package-hash".into()),
            frozen_package: None,
            authorization: None,
        };
        let directory = root.join("helper-libraries");
        fs::create_dir_all(&directory).unwrap();
        let key = storage::sha256(&serde_json::to_vec(&(&holder, &lease.template_id)).unwrap());
        let file = LeaseFile {
            path: directory.join(format!("{key}.json")),
            lease,
        };
        file.checkpoint().unwrap();
        file
    }

    #[test]
    fn removed_generation_rotates_after_archiving_exact_before_image() {
        // Both paths matter: normal rotation, and a crash after the history write.
        for history_already_written in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut file = removed_fixture(root.path());
            let before = file.lease.clone();
            let bytes = serde_json::to_vec_pretty(&before).unwrap();
            let archive = file
                .path
                .parent()
                .unwrap()
                .join("history")
                .join(format!("{}.json", before.instance.global_id));
            if history_already_written {
                fs::create_dir_all(archive.parent().unwrap()).unwrap();
                fs::write(&archive, &bytes).unwrap();
            }

            file.rotate_removed().unwrap();
            assert_eq!(fs::read(&archive).unwrap(), bytes);
            let current: LibraryLease =
                serde_json::from_slice(&fs::read(&file.path).unwrap()).unwrap();
            assert_ne!(current.instance.global_id, before.instance.global_id);
            assert_ne!(current.instance.library_id, before.instance.library_id);
            assert_ne!(current.owner_marker, before.owner_marker);
            assert_eq!(current.instance.library_id.len(), 30);
            assert_eq!(current.holder, before.holder);
            assert_eq!(current.template_id, before.template_id);
            assert_eq!(current.plugin_hash, before.plugin_hash);
            assert_eq!(current.endpoint, before.endpoint);
            assert_eq!(current.company, before.company);
            assert_eq!(current.caller, before.caller);
            assert_eq!(current.phase, "reserved");
            assert_eq!(current.transitions, vec!["reserved"]);
            assert!(current.package_sha256.is_none());
            let persisted = fs::read(&file.path).unwrap();
            assert!(file.rotate_removed().is_err());
            assert_eq!(fs::read(&file.path).unwrap(), persisted);
        }
    }

    #[test]
    fn conflicting_history_preserves_removed_current_generation() {
        let root = tempfile::tempdir().unwrap();
        let mut file = removed_fixture(root.path());
        let before = fs::read(&file.path).unwrap();
        let archive = file
            .path
            .parent()
            .unwrap()
            .join("history")
            .join(format!("{}.json", file.lease.instance.global_id));
        fs::create_dir_all(archive.parent().unwrap()).unwrap();
        let conflict = b"existing history belongs to something else";
        fs::write(&archive, conflict).unwrap();
        assert!(file.rotate_removed().is_err());
        assert_eq!(fs::read(&file.path).unwrap(), before);
        assert_eq!(fs::read(&archive).unwrap(), conflict);
        assert_eq!(file.lease.phase, "removed");
    }

    #[test]
    fn other_holders_future_schema_does_not_block_owned_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let file = removed_fixture(root.path());
        let unrelated = json!({
            "holder": uuid::Uuid::new_v4().to_string(),
            "futureSchemaField": true
        });
        fs::write(
            root.path().join("helper-libraries/unrelated.json"),
            serde_json::to_vec(&unrelated).unwrap(),
        )
        .unwrap();
        let (owned, diagnostics) = for_holder(root.path(), &file.lease.holder).unwrap();
        assert!(diagnostics.is_empty());
        assert_eq!(owned.len(), 1);
        assert_eq!(
            owned[0].lease.instance.global_id,
            file.lease.instance.global_id
        );
    }

    #[test]
    fn malformed_record_is_reported_without_hiding_valid_owned_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let file = removed_fixture(root.path());
        let malformed_path = root.path().join("helper-libraries/malformed.json");
        let malformed = b"{invalid-json";
        fs::write(&malformed_path, malformed).unwrap();
        let (owned, diagnostics) = for_holder(root.path(), &file.lease.holder).unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(
            owned[0].lease.instance.global_id,
            file.lease.instance.global_id
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "HELPER_OWNERSHIP_STATE_FAILED");
        assert_eq!(fs::read(&malformed_path).unwrap(), malformed);
        assert_eq!(
            fs::read(&file.path).unwrap(),
            serde_json::to_vec_pretty(&file.lease).unwrap()
        );
    }
}
