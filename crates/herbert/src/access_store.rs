//! Share access ownership independently of profiles, journals and --state-dir.
use crate::access::{AccessRegistry, LockedRegistry, MembershipPhase};
use crate::storage::{atomic_write, sha256};
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct AccessStore {
    root: Result<PathBuf, Diagnostic>,
    legacy_root: PathBuf,
}
impl AccessStore {
    pub(crate) fn default_location(legacy_root: PathBuf) -> Self {
        // Portable installations keep coordination with their application state.
        // This local store does not establish server helper liveness.
        let root = absolute(legacy_root.join("coordination"));
        // New portable stores have no legacy redirect. An absolute redirect
        // would bind the installation to its old path after the folder moves.
        Self { root, legacy_root: legacy_root.join("coordination") }
    }
    pub(crate) fn explicit(root: PathBuf, legacy_root: PathBuf) -> Self {
        Self {
            root: absolute(root),
            legacy_root,
        }
    }
    pub(crate) fn location(&self) -> Result<PathBuf, Diagnostic> {
        self.root.clone()
    }
    /// Read ownership without creating directories, locks, redirects, migration
    /// intents or checkpoints. A pending migration needs explicit reconciliation;
    /// a read-only preview must never perform it as a side effect.
    pub(crate) fn snapshot(&self, endpoint:&str, caller:&str) -> Result<AccessRegistry,Diagnostic> {
        let root=self.location()?;
        for directory in [&root,&root.join("access"),&root.join("access-migrations")] {reject_link(directory)?;}
        let key=account_key(endpoint,caller);
        if read_optional(&root.join("access-migrations").join(format!("{key}.json")))?.is_some() {
            return Err(conflict("Access ownership has a pending migration. Preview preserved it without reconciliation."));
        }
        let mut current=match read_optional(&source_path(&root,&key))? {
            Some(bytes)=>decode_registry(&bytes,endpoint,caller)?,
            None=>AccessRegistry{schema_version:1,endpoint:endpoint.into(),caller:caller.into(),memberships:Default::default(),leases:Default::default()},
        };
        reject_link(&self.legacy_root)?;reject_link(&self.legacy_root.join("access"))?;
        let same=self.legacy_root==root || (root.exists()&&self.legacy_root.exists()&&canonical(&root)?==canonical(&self.legacy_root)?);
        if !same {
            if let Some(bytes)=read_optional(&source_path(&self.legacy_root,&key))? {
                if let Some(redirect)=decode_redirect(&bytes)? {
                    // verify_redirect also verifies/creates history directories;
                    // preview validates identity/routing only and creates nothing.
                    if redirect.schema_version!=1 || redirect.kind!="herbertAccessRedirect" || redirect.endpoint!=endpoint || redirect.caller!=caller || redirect.coordination_root!=canonical(&root)? {
                        return Err(conflict("The legacy access redirect does not identify this ownership store."));
                    }
                } else {current=merge(&current,&decode_registry(&bytes,endpoint,caller)?)?;}
            }
        }
        Ok(current)
    }
    /// Call from spawn_blocking. The account lock survives in the returned guard.
    pub(crate) fn wait(&self, endpoint: &str, caller: &str) -> Result<LockedRegistry, Diagnostic> {
        let root = self.location()?;
        ensure_directory(&root)?;
        let root = canonical(&root)?;
        let mut current = LockedRegistry::wait(&root, endpoint, caller)?;
        let key = account_key(endpoint, caller);
        let pending_path = pending_path(&root, &key)?;
        if let Some(bytes) = read_optional(&pending_path)? {
            let pending: Pending = serde_json::from_slice(&bytes).map_err(|_| {
                conflict("The pending access migration is invalid; preserve its evidence.")
            })?;
            resume(&root, &key, &pending_path, &mut current, pending)?;
        }
        ensure_directory(&self.legacy_root)?;
        let legacy_root = canonical(&self.legacy_root)?;
        if legacy_root == root {
            return Ok(current);
        }
        let _legacy_lock = source_lock(&legacy_root, &key)?;
        let source = source_path(&legacy_root, &key);
        match read_optional(&source)? {
            None => write_redirect(&source, &root, endpoint, caller, None)?,
            Some(bytes) => {
                if let Some(redirect) = decode_redirect(&bytes)? {
                    verify_redirect(&redirect, &root, endpoint, caller)?;
                    return Ok(current);
                }
                let legacy = decode_registry(&bytes, endpoint, caller)?;
                let merged = merge(&current.registry, &legacy).map_err(|mut diagnostic| {
                    diagnostic.message = format!(
                        "{} Shared evidence: '{}'. Legacy evidence: '{}'.",
                        diagnostic.message,
                        source_path(&root, &key).display(),
                        source.display()
                    );
                    diagnostic
                })?;
                let source_hash = sha256(&bytes);
                let archive = archive_source(&root, &key, &source_hash, &bytes)?;
                if terminal(&legacy) {
                    // Only history is copied. Repeating this merge after a crash
                    // is idempotent. No live grant ownership is transferred.
                    current.registry = merged;
                    current.checkpoint()?;
                    write_redirect(
                        &source,
                        &root,
                        endpoint,
                        caller,
                        Some((&archive, &source_hash)),
                    )?;
                } else {
                    let pending = Pending {
                        schema_version: 1,
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        endpoint: endpoint.into(),
                        caller: caller.into(),
                        source_root: legacy_root.clone(),
                        source_sha256: source_hash,
                        target_before_sha256: registry_hash(&current.registry)?,
                        target_after_sha256: registry_hash(&merged)?,
                        target_after: merged,
                    };
                    atomic_write(&pending_path, &encode(&pending)?)?;
                    // The source lock is already held. All other clients see
                    // this descriptor before using the shared account registry.
                    finish_locked(&root, &key, &pending_path, &mut current, &pending)?;
                }
            }
        }
        Ok(current)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    operation_id: String,
    endpoint: String,
    caller: String,
    source_root: PathBuf,
    source_sha256: String,
    target_before_sha256: String,
    target_after_sha256: String,
    target_after: AccessRegistry,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Redirect {
    schema_version: u32,
    kind: String,
    endpoint: String,
    caller: String,
    coordination_root: PathBuf,
    original_archive: Option<PathBuf>,
    original_sha256: Option<String>,
}
fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn conflict(message: &str) -> Diagnostic {
    error("ACCESS_COORDINATION_MIGRATION_CONFLICT", message)
}
fn failed(message: &str) -> Diagnostic {
    error("ACCESS_STATE_UNAVAILABLE", message)
}
fn absolute(path: PathBuf) -> Result<PathBuf, Diagnostic> {
    if path.is_absolute() {
        Ok(path)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|_| failed("Cannot resolve the coordination directory."))
    }
}
fn canonical(path: &Path) -> Result<PathBuf, Diagnostic> {
    fs::canonicalize(path).map_err(|_| failed("Cannot resolve the access ownership directory."))
}
fn reject_link(path: &Path) -> Result<(), Diagnostic> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(error(
            "UNSAFE_PATH",
            "Access ownership and migration files cannot be symbolic links or junctions.",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(failed("Cannot inspect access ownership evidence.")),
    }
}
fn ensure_directory(path: &Path) -> Result<(), Diagnostic> {
    reject_link(path)?;
    fs::create_dir_all(path)
        .map_err(|_| failed("Cannot create the access ownership directory."))?;
    reject_link(path)
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, Diagnostic> {
    reject_link(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failed("Cannot read access ownership evidence.")),
    }
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Diagnostic> {
    serde_json::to_vec_pretty(value).map_err(|_| failed("Cannot encode access ownership evidence."))
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn registry_hash(value: &AccessRegistry) -> Result<String, Diagnostic> {
    Ok(sha256(&encode(value)?))
}
fn account_key(endpoint: &str, caller: &str) -> String {
    sha256(&serde_json::to_vec(&(endpoint, caller)).expect("two strings serialize"))
}
fn source_path(root: &Path, key: &str) -> PathBuf {
    root.join("access").join(format!("{key}.json"))
}
fn source_lock(root: &Path, key: &str) -> Result<File, Diagnostic> {
    let directory = root.join("access");
    ensure_directory(&directory)?;
    let path = directory.join(format!("{key}.lock"));
    reject_link(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| failed("Cannot open the legacy access account lock."))?;
    file.lock_exclusive()
        .map_err(|_| failed("Cannot lock legacy access ownership."))?;
    Ok(file)
}
fn pending_path(root: &Path, key: &str) -> Result<PathBuf, Diagnostic> {
    let directory = root.join("access-migrations");
    ensure_directory(&directory)?;
    Ok(directory.join(format!("{key}.json")))
}
fn history_directory(root: &Path, key: &str) -> Result<PathBuf, Diagnostic> {
    let directory = root.join("access-history").join(key);
    ensure_directory(&directory)?;
    Ok(directory)
}
fn store_exact(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    match read_optional(path)? {
        Some(existing) if existing != bytes => Err(conflict(
            "Existing migration history differs; preserve both ownership records.",
        )),
        Some(_) => Ok(()),
        None => atomic_write(path, bytes),
    }
}
fn archive_source(root: &Path, key: &str, hash: &str, bytes: &[u8]) -> Result<PathBuf, Diagnostic> {
    if sha256(bytes) != hash {
        return Err(conflict(
            "The legacy ownership archive hash does not match.",
        ));
    }
    let path = history_directory(root, key)?.join(format!("{hash}.json"));
    store_exact(&path, bytes)?;
    Ok(path)
}
fn decode_registry(
    bytes: &[u8],
    endpoint: &str,
    caller: &str,
) -> Result<AccessRegistry, Diagnostic> {
    let registry: AccessRegistry = serde_json::from_slice(bytes).map_err(|_| {
        conflict("The legacy access registry is invalid; preserve it for reconciliation.")
    })?;
    if registry.schema_version != 1 || registry.endpoint != endpoint || registry.caller != caller {
        return Err(conflict(
            "Legacy access ownership does not match this endpoint and caller.",
        ));
    }
    Ok(registry)
}
fn decode_redirect(bytes: &[u8]) -> Result<Option<Redirect>, Diagnostic> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| conflict("The legacy access file is invalid."))?;
    if value.get("kind").is_none() {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| conflict("The legacy access redirect is invalid."))
}
fn verify_redirect(
    redirect: &Redirect,
    root: &Path,
    endpoint: &str,
    caller: &str,
) -> Result<(), Diagnostic> {
    if redirect.schema_version != 1
        || redirect.kind != "herbertAccessRedirect"
        || redirect.coordination_root != root
        || redirect.endpoint != endpoint
        || redirect.caller != caller
    {
        return Err(conflict(
            "The legacy access file points to a different coordination store. Use its recorded --coordination-dir and preserve the original evidence.",
        ));
    }
    match (&redirect.original_archive, &redirect.original_sha256) {
        (Some(path), Some(hash)) => {
            if !valid_hash(hash)
                || *path
                    != history_directory(root, &account_key(endpoint, caller))?
                        .join(format!("{hash}.json"))
            {
                return Err(conflict(
                    "The redirect archive path does not match its fixed account and hash.",
                ));
            }
            let bytes = read_optional(path)?
                .ok_or_else(|| conflict("The original access ownership archive is missing."))?;
            if sha256(&bytes) != *hash {
                return Err(conflict("The original access ownership archive changed."));
            }
        }
        (None, None) => (),
        _ => {
            return Err(conflict(
                "The legacy access redirect omits its matching archive evidence.",
            ));
        }
    }
    Ok(())
}
fn write_redirect(
    path: &Path,
    root: &Path,
    endpoint: &str,
    caller: &str,
    archive: Option<(&Path, &str)>,
) -> Result<(), Diagnostic> {
    let redirect = Redirect {
        schema_version: 1,
        kind: "herbertAccessRedirect".into(),
        endpoint: endpoint.into(),
        caller: caller.into(),
        coordination_root: root.into(),
        original_archive: archive.map(|(p, _)| p.into()),
        original_sha256: archive.map(|(_, h)| h.into()),
    };
    reject_link(path)?;
    atomic_write(path, &encode(&redirect)?)
}
fn terminal(registry: &AccessRegistry) -> bool {
    registry
        .memberships
        .values()
        .all(|m| m.phase == MembershipPhase::Released && m.holders.is_empty())
        && registry
            .leases
            .values()
            .all(|l| matches!(l.state.as_str(), "released" | "preexisting" | "notGranted"))
}
fn merge(current: &AccessRegistry, legacy: &AccessRegistry) -> Result<AccessRegistry, Diagnostic> {
    let mut merged = current.clone();
    for (id, lease) in &legacy.leases {
        match merged.leases.get(id) {
            Some(existing) if encode(existing)? != encode(lease)? => {
                return Err(conflict(
                    "A lease ID has conflicting legacy and shared ownership. Preserve both records and reconcile that holder.",
                ));
            }
            Some(_) => (),
            None => {
                merged.leases.insert(id.clone(), lease.clone());
            }
        }
    }
    for (group, member) in &legacy.memberships {
        match merged.memberships.get(group) {
            Some(existing) if encode(existing)? == encode(member)? => (),
            Some(_) if member.phase == MembershipPhase::Released && member.holders.is_empty() => (),
            Some(existing)
                if existing.phase == MembershipPhase::Released && existing.holders.is_empty() =>
            {
                merged.memberships.insert(group.clone(), member.clone());
            }
            Some(_) => {
                return Err(conflict(
                    "A group has conflicting live legacy and shared grant intents. Preserve both records; do not merge or remove its membership.",
                ));
            }
            None => {
                merged.memberships.insert(group.clone(), member.clone());
            }
        }
    }
    Ok(merged)
}
fn resume(
    root: &Path,
    key: &str,
    path: &Path,
    current: &mut LockedRegistry,
    pending: Pending,
) -> Result<(), Diagnostic> {
    reject_link(&pending.source_root)?;
    if canonical(&pending.source_root)? != pending.source_root {
        return Err(conflict(
            "The pending migration source path is not canonical.",
        ));
    }
    if canonical(&pending.source_root)? == root {
        return Err(conflict(
            "The migration source cannot be the shared registry itself.",
        ));
    }
    let _source_lock = source_lock(&pending.source_root, key)?;
    finish_locked(root, key, path, current, &pending)
}
fn finish_locked(
    root: &Path,
    key: &str,
    path: &Path,
    current: &mut LockedRegistry,
    pending: &Pending,
) -> Result<(), Diagnostic> {
    if pending.schema_version != 1
        || uuid::Uuid::parse_str(&pending.operation_id).is_err()
        || !valid_hash(&pending.source_sha256)
        || !valid_hash(&pending.target_before_sha256)
        || !valid_hash(&pending.target_after_sha256)
        || pending.endpoint != current.registry.endpoint
        || pending.caller != current.registry.caller
        || pending.endpoint != pending.target_after.endpoint
        || pending.caller != pending.target_after.caller
        || pending.target_after.schema_version != 1
        || registry_hash(&pending.target_after)? != pending.target_after_sha256
    {
        return Err(conflict(
            "Pending access migration identity or target evidence does not match.",
        ));
    }
    let archive = history_directory(root, key)?.join(format!("{}.json", pending.source_sha256));
    let original = read_optional(&archive)?
        .ok_or_else(|| conflict("Pending migration lacks its original ownership archive."))?;
    if sha256(&original) != pending.source_sha256 {
        return Err(conflict(
            "Pending migration's original ownership archive changed.",
        ));
    }
    decode_registry(&original, &pending.endpoint, &pending.caller)?;
    let source = source_path(&pending.source_root, key);
    let bytes = read_optional(&source)?
        .ok_or_else(|| conflict("Pending migration's legacy source is missing."))?;
    let already_redirected = if let Some(redirect) = decode_redirect(&bytes)? {
        verify_redirect(&redirect, root, &pending.endpoint, &pending.caller)?;
        if redirect.original_sha256.as_deref() != Some(pending.source_sha256.as_str()) {
            return Err(conflict(
                "The redirect identifies another legacy before-image.",
            ));
        }
        true
    } else {
        if sha256(&bytes) != pending.source_sha256 {
            return Err(conflict(
                "Legacy ownership changed during the interrupted migration. Preserve both records; no native write is authorized.",
            ));
        }
        false
    };
    let actual_hash = registry_hash(&current.registry)?;
    if actual_hash != pending.target_after_sha256 {
        if already_redirected || actual_hash != pending.target_before_sha256 {
            return Err(conflict(
                "Shared ownership changed during the interrupted migration. Preserve both records; no native write is authorized.",
            ));
        }
        current.registry = pending.target_after.clone();
        current.checkpoint()?;
    }
    if !already_redirected {
        write_redirect(
            &source,
            root,
            &pending.endpoint,
            &pending.caller,
            Some((&archive, &pending.source_sha256)),
        )?;
    }
    let completed =
        history_directory(root, key)?.join(format!("migration-{}.json", pending.operation_id));
    store_exact(&completed, &encode(pending)?)?;
    fs::remove_file(path).map_err(|_| failed("Migration was completed but its pending marker could not be removed; retry readback reconciliation."))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Membership;
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    fn preview_snapshot_creates_nothing_and_preserves_active_legacy_ownership() {
        let directory=tempfile::tempdir().unwrap();let shared=directory.path().join("new");let old=directory.path().join("old");
        let store=AccessStore::explicit(shared.clone(),old.clone());
        assert!(store.snapshot("https://fixture.invalid/","manager").unwrap().memberships.is_empty());assert!(!shared.exists());assert!(!old.exists());
        let value=registry(true);let bytes=legacy(&old,&value);let key=account_key(&value.endpoint,&value.caller);
        let before_files=fs::read_dir(old.join("access")).unwrap().map(|entry|{let p=entry.unwrap().path();let b=fs::read(&p).unwrap();(p,b)}).collect::<BTreeMap<_,_>>();
        let snapshot=store.snapshot(&value.endpoint,&value.caller).unwrap();assert_eq!(encode(&snapshot).unwrap(),encode(&value).unwrap());
        assert_eq!(fs::read(source_path(&old,&key)).unwrap(),bytes);assert!(!shared.exists());
        let after_files=fs::read_dir(old.join("access")).unwrap().map(|entry|{let p=entry.unwrap().path();let b=fs::read(&p).unwrap();(p,b)}).collect::<BTreeMap<_,_>>();
        assert_eq!(before_files,after_files); // no new lock, redirect or changed original bytes
    }
    #[test]
    fn preview_snapshot_refuses_pending_migration_without_resuming_or_rewriting_it() {
        let directory=tempfile::tempdir().unwrap();let shared=directory.path().join("shared");let old=directory.path().join("old");let value=registry(true);let bytes=legacy(&old,&value);
        let key=account_key(&value.endpoint,&value.caller);let pending=pending_path(&shared,&key).unwrap();fs::write(&pending,b"pending-original").unwrap();
        let store=AccessStore::explicit(shared.clone(),old.clone());assert!(store.snapshot(&value.endpoint,&value.caller).is_err());
        assert_eq!(fs::read(&pending).unwrap(),b"pending-original");assert_eq!(fs::read(source_path(&old,&key)).unwrap(),bytes);assert!(!shared.join("access").exists());
    }
    #[test]
    fn default_coordination_stays_inside_the_selected_portable_state() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("app").join("data");
        assert_eq!(AccessStore::default_location(state.clone()).location().unwrap(),
            state.join("coordination"));
    }

    #[test]
    fn default_store_can_move_without_an_absolute_redirect() {
        let directory = tempfile::tempdir().unwrap();
        let before = directory.path().join("before");
        let after = directory.path().join("after");
        {
            let store = AccessStore::default_location(before.clone());
            let _guard = store.wait("https://fixture.invalid/", "manager").unwrap();
        }
        assert!(!before.join("access").exists());
        std::fs::rename(&before, &after).unwrap();
        let store = AccessStore::default_location(after.clone());
        let _guard = store.wait("https://fixture.invalid/", "manager").unwrap();
        assert!(!before.exists());
        assert!(!after.join("access").exists());
    }

    fn registry(active: bool) -> AccessRegistry {
        AccessRegistry {
            schema_version: 1,
            endpoint: "https://fixture.invalid/".into(),
            caller: "manager".into(),
            leases: BTreeMap::new(),
            memberships: BTreeMap::from([(
                "OWNED".into(),
                Membership {
                    group: "OWNED".into(),
                    holders: if active {
                        BTreeSet::from(["holder-a".into()])
                    } else {
                        BTreeSet::new()
                    },
                    phase: if active {
                        MembershipPhase::Active
                    } else {
                        MembershipPhase::Released
                    },
                    existed_before: false,
                    membership_added: true,
                    before_revision: "1".into(),
                    grant_revision: Some("2".into()),
                    intent_id: "intent-a".into(),
                    lease_ids: BTreeSet::new(),
                    native_write_confirmed: true,
                    account_row_id: Some("row-a".into()),
                    account_safeguards_sha256: Some("safe-a".into()),
                },
            )]),
        }
    }
    fn legacy(root: &Path, value: &AccessRegistry) -> Vec<u8> {
        let mut locked = LockedRegistry::wait(root, &value.endpoint, &value.caller).unwrap();
        locked.registry = value.clone();
        locked.checkpoint().unwrap();
        encode(value).unwrap()
    }
    #[test]
    fn distinct_state_directories_share_one_account_registry() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("state-a");
        let b = root.path().join("state-b");
        let shared = root.path().join("shared");
        let one = AccessStore::explicit(shared.clone(), a);
        let two = AccessStore::explicit(shared, b);
        let value = registry(true);
        {
            let mut locked = one.wait(&value.endpoint, &value.caller).unwrap();
            locked.registry = value.clone();
            locked.checkpoint().unwrap();
        }
        let locked = two.wait(&value.endpoint, &value.caller).unwrap();
        assert_eq!(encode(&locked.registry).unwrap(), encode(&value).unwrap());
    }
    #[test]
    fn terminal_history_is_preserved_and_redirected() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let shared = root.path().join("shared");
        let value = registry(false);
        let before = legacy(&old, &value);
        let store = AccessStore::explicit(shared.clone(), old.clone());
        let current = store.wait(&value.endpoint, &value.caller).unwrap();
        assert_eq!(encode(&current.registry).unwrap(), before);
        let key = account_key(&value.endpoint, &value.caller);
        let marker = decode_redirect(&fs::read(source_path(&old, &key)).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(marker.original_archive.unwrap()).unwrap(), before);
        assert!(!pending_path(&shared, &key).unwrap().exists());
    }
    #[test]
    fn active_adoption_keeps_exact_holder_and_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let shared = root.path().join("shared");
        let value = registry(true);
        let before = legacy(&old, &value);
        let store = AccessStore::explicit(shared.clone(), old.clone());
        {
            let current = store.wait(&value.endpoint, &value.caller).unwrap();
            assert_eq!(encode(&current.registry).unwrap(), before);
        }
        let other = AccessStore::explicit(shared.clone(), root.path().join("different-state"));
        let current = other.wait(&value.endpoint, &value.caller).unwrap();
        assert_eq!(encode(&current.registry).unwrap(), before);
        let key = account_key(&value.endpoint, &value.caller);
        let marker = decode_redirect(&fs::read(source_path(&old, &key)).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(marker.original_archive.unwrap()).unwrap(), before);
    }
    #[test]
    fn pending_adoption_recovers_from_each_durable_boundary() {
        for boundary in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let old = root.path().join("old");
            let shared = root.path().join("shared");
            let value = registry(true);
            let before = legacy(&old, &value);
            ensure_directory(&shared).unwrap();
            let shared = canonical(&shared).unwrap();
            let old = canonical(&old).unwrap();
            let key = account_key(&value.endpoint, &value.caller);
            let mut current =
                LockedRegistry::wait(&shared, &value.endpoint, &value.caller).unwrap();
            let archive = archive_source(&shared, &key, &sha256(&before), &before).unwrap();
            let pending = Pending {
                schema_version: 1,
                operation_id: uuid::Uuid::new_v4().to_string(),
                endpoint: value.endpoint.clone(),
                caller: value.caller.clone(),
                source_root: old.clone(),
                source_sha256: sha256(&before),
                target_before_sha256: registry_hash(&current.registry).unwrap(),
                target_after_sha256: registry_hash(&value).unwrap(),
                target_after: value.clone(),
            };
            atomic_write(
                &pending_path(&shared, &key).unwrap(),
                &encode(&pending).unwrap(),
            )
            .unwrap();
            if boundary >= 1 {
                current.registry = value.clone();
                current.checkpoint().unwrap();
            }
            if boundary >= 2 {
                write_redirect(
                    &source_path(&old, &key),
                    &shared,
                    &value.endpoint,
                    &value.caller,
                    Some((&archive, &pending.source_sha256)),
                )
                .unwrap();
            }
            drop(current);
            let store = AccessStore::explicit(shared.clone(), root.path().join("new-state"));
            let current = store.wait(&value.endpoint, &value.caller).unwrap();
            assert_eq!(encode(&current.registry).unwrap(), before);
            assert!(!pending_path(&shared, &key).unwrap().exists());
        }
    }
    #[test]
    fn conflicting_live_grants_preserve_both_original_records() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let shared = root.path().join("shared");
        let value = registry(true);
        let before = legacy(&old, &value);
        let mut other = value.clone();
        other.memberships.get_mut("OWNED").unwrap().intent_id = "different-intent".into();
        let primary = legacy(&shared, &other);
        let store = AccessStore::explicit(shared.clone(), old.clone());
        let error = match store.wait(&value.endpoint, &value.caller) {
            Ok(_) => panic!("conflicting ownership was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, "ACCESS_COORDINATION_MIGRATION_CONFLICT");
        let key = account_key(&value.endpoint, &value.caller);
        assert_eq!(fs::read(source_path(&old, &key)).unwrap(), before);
        assert_eq!(fs::read(source_path(&shared, &key)).unwrap(), primary);
    }
    #[test]
    fn interrupted_source_change_blocks_adoption_and_preserves_evidence() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old");
        let shared = root.path().join("shared");
        let value = registry(true);
        let before = legacy(&old, &value);
        ensure_directory(&shared).unwrap();
        let shared = canonical(&shared).unwrap();
        let old = canonical(&old).unwrap();
        let key = account_key(&value.endpoint, &value.caller);
        let current = LockedRegistry::wait(&shared, &value.endpoint, &value.caller).unwrap();
        let original_primary = encode(&current.registry).unwrap();
        let archive = archive_source(&shared, &key, &sha256(&before), &before).unwrap();
        let pending = Pending {
            schema_version: 1,
            operation_id: uuid::Uuid::new_v4().to_string(),
            endpoint: value.endpoint.clone(),
            caller: value.caller.clone(),
            source_root: old.clone(),
            source_sha256: sha256(&before),
            target_before_sha256: registry_hash(&current.registry).unwrap(),
            target_after_sha256: registry_hash(&value).unwrap(),
            target_after: value.clone(),
        };
        let pending_file = pending_path(&shared, &key).unwrap();
        atomic_write(&pending_file, &encode(&pending).unwrap()).unwrap();
        drop(current);
        let mut changed = value.clone();
        changed
            .memberships
            .get_mut("OWNED")
            .unwrap()
            .holders
            .insert("holder-b".into());
        let changed_bytes = legacy(&old, &changed);
        let store = AccessStore::explicit(shared.clone(), root.path().join("other-state"));
        let error = match store.wait(&value.endpoint, &value.caller) {
            Ok(_) => panic!("changed legacy ownership was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, "ACCESS_COORDINATION_MIGRATION_CONFLICT");
        assert!(pending_file.exists());
        assert_eq!(fs::read(archive).unwrap(), before);
        assert_eq!(fs::read(source_path(&old, &key)).unwrap(), changed_bytes);
        let current = LockedRegistry::wait(&shared, &value.endpoint, &value.caller).unwrap();
        assert_eq!(encode(&current.registry).unwrap(), original_primary);
    }
}
