//! Ownership rules for the explicitly authorized temporary-membership workflow.
use fs2::FileExt;
use herbert_contract::{Diagnostic, Identity};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MembershipPhase {
    GrantPending,
    Active,
    CleanupPending,
    Released,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Membership {
    pub group: String,
    pub holders: BTreeSet<String>,
    pub phase: MembershipPhase,
    pub existed_before: bool,
    pub membership_added: bool,
    pub before_revision: String,
    pub grant_revision: Option<String>,
    pub intent_id: String,
    pub lease_ids: BTreeSet<String>,
    #[serde(default)]
    pub native_write_confirmed: bool,
    #[serde(default)]
    pub account_row_id: Option<String>,
    #[serde(default)]
    pub account_safeguards_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessRegistry {
    pub schema_version: u32,
    pub endpoint: String,
    pub caller: String,
    pub memberships: BTreeMap<String, Membership>,
    pub leases: BTreeMap<String, AccessLease>,
}

pub struct LockedRegistry {
    path: PathBuf,
    _lock: File,
    pub registry: AccessRegistry,
}
fn access_error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
impl LockedRegistry {
    // Memberships belong to an endpoint/caller, across company and destination
    // choices. The account lock prevents another Herbert process from granting or
    // cleaning this caller's groups while the native write/readback is pending.
    pub fn open(state_root: &Path, endpoint: &str, caller: &str) -> Result<Self, Diagnostic> {
        Self::open_with_lock(state_root, endpoint, caller, false)
    }
    /// Wait off the async executor when another accepted operation owns access.
    pub(crate) fn wait(
        state_root: &Path,
        endpoint: &str,
        caller: &str,
    ) -> Result<Self, Diagnostic> {
        Self::open_with_lock(state_root, endpoint, caller, true)
    }
    fn open_with_lock(
        state_root: &Path,
        endpoint: &str,
        caller: &str,
        wait: bool,
    ) -> Result<Self, Diagnostic> {
        let directory = state_root.join("access");
        fs::create_dir_all(&directory).map_err(|_| {
            access_error(
                "ACCESS_STATE_UNAVAILABLE",
                "Cannot create the temporary-access registry.",
            )
        })?;
        if fs::symlink_metadata(&directory)
            .map_err(|_| {
                access_error(
                    "ACCESS_STATE_UNAVAILABLE",
                    "Cannot inspect the access registry.",
                )
            })?
            .file_type()
            .is_symlink()
        {
            return Err(access_error(
                "UNSAFE_PATH",
                "The access registry cannot be a symbolic link or junction.",
            ));
        }
        let key = crate::storage::sha256(&serde_json::to_vec(&(endpoint, caller)).unwrap());
        let path = directory.join(format!("{key}.json"));
        let lock_path = directory.join(format!("{key}.lock"));
        for path in [&path, &lock_path] {
            if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(access_error(
                    "UNSAFE_PATH",
                    "Access state and lock files cannot be symbolic links.",
                ));
            }
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|_| {
                access_error(
                    "ACCESS_STATE_UNAVAILABLE",
                    "Cannot open the access registry lock.",
                )
            })?;
        let acquired = if wait {
            lock.lock_exclusive()
        } else {
            lock.try_lock_exclusive()
        };
        acquired.map_err(|_| {
            access_error(
                "ACCESS_BUSY",
                "Another Herbert process is reconciling temporary access for this Epicor user.",
            )
        })?;
        let registry = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<AccessRegistry>(&bytes).map_err(|_| {
                access_error(
                    "ACCESS_STATE_INVALID",
                    "Preserve the invalid registry; Epicor membership writes are refused.",
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AccessRegistry {
                schema_version: 1,
                endpoint: endpoint.into(),
                caller: caller.into(),
                memberships: BTreeMap::new(),
                leases: BTreeMap::new(),
            },
            Err(_) => {
                return Err(access_error(
                    "ACCESS_STATE_UNAVAILABLE",
                    "Cannot read temporary-access ownership evidence.",
                ));
            }
        };
        if registry.schema_version != 1
            || registry.endpoint != endpoint
            || registry.caller != caller
        {
            return Err(access_error(
                "ACCESS_SCOPE_MISMATCH",
                "The registry does not match the fixed endpoint and Epicor user.",
            ));
        }
        Ok(Self {
            path,
            _lock: lock,
            registry,
        })
    }
    pub fn checkpoint(&self) -> Result<(), Diagnostic> {
        crate::storage::atomic_write(
            &self.path,
            &serde_json::to_vec_pretty(&self.registry).map_err(|_| {
                access_error(
                    "ACCESS_STATE_FAILED",
                    "Cannot encode access ownership evidence.",
                )
            })?,
        )
    }
    pub fn request(
        &mut self,
        run_id: &str,
        mut lease: AccessLease,
        current: &str,
        revision: &str,
    ) -> Result<bool, Diagnostic> {
        if lease.schema_version != 1 || run_id.is_empty() || lease.lease_id.is_empty() {
            return Err(access_error(
                "INVALID_ACCESS_LEASE",
                "A lease needs schemaVersion 1 and exact nonempty run and lease identities.",
            ));
        }
        if self.registry.leases.contains_key(&lease.lease_id) {
            return Err(access_error(
                "ACCESS_LEASE_ID_REUSED",
                "This lease identity already has durable ownership evidence. Reconcile it before issuing a new intent.",
            ));
        }
        if lease.endpoint != self.registry.endpoint || lease.caller != self.registry.caller {
            return Err(access_error(
                "ACCESS_SCOPE_MISMATCH",
                "A lease belongs to a different endpoint or Epicor user.",
            ));
        }
        lease.holder = run_id.into();
        let (_, missing) = append_owned_group(current, &lease.group)?;
        if let Some(existing) = self
            .registry
            .memberships
            .get_mut(&lease.group)
            .filter(|m| m.phase != MembershipPhase::Released)
        {
            if existing.phase != MembershipPhase::Active {
                return Err(access_error(
                    "ACCESS_RECONCILIATION_REQUIRED",
                    "An interrupted membership intent needs independent readback before new grants or cleanup.",
                ));
            }
            if missing {
                return Err(access_error(
                    "ACCESS_MEMBERSHIP_CHANGED",
                    "A claimed membership disappeared. Preserve its ownership evidence and reconcile it before continuing.",
                ));
            }
            existing.holders.insert(run_id.into());
            existing.lease_ids.insert(lease.lease_id.clone());
            lease.existed_before = existing.existed_before;
            lease.membership_added = existing.membership_added;
            lease.state = "active".into();
            self.registry.leases.insert(lease.lease_id.clone(), lease);
            self.checkpoint()?;
            return Ok(false);
        }
        lease.existed_before = !missing;
        lease.membership_added = false;
        lease.state = if missing { "grantPending" } else { "active" }.into();
        let membership = Membership {
            group: lease.group.clone(),
            holders: BTreeSet::from([run_id.into()]),
            phase: if missing {
                MembershipPhase::GrantPending
            } else {
                MembershipPhase::Active
            },
            existed_before: !missing,
            membership_added: false,
            before_revision: revision.into(),
            grant_revision: None,
            intent_id: uuid::Uuid::new_v4().to_string(),
            lease_ids: BTreeSet::from([lease.lease_id.clone()]),
            native_write_confirmed: false,
            account_row_id: None,
            account_safeguards_sha256: None,
        };
        self.registry
            .memberships
            .insert(lease.group.clone(), membership);
        self.registry.leases.insert(lease.lease_id.clone(), lease);
        self.checkpoint()?;
        Ok(missing)
    }

    pub(crate) fn record_account_proof(
        &mut self,
        group: &str,
        row_id: &str,
        safeguards_sha256: &str,
    ) -> Result<(), Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error("ACCESS_LEASE_NOT_FOUND", "No owned grant intent exists.")
        })?;
        member.account_row_id = Some(row_id.into());
        member.account_safeguards_sha256 = Some(safeguards_sha256.into());
        self.checkpoint()
    }

    pub(crate) fn record_grant_response(&mut self, group: &str) -> Result<(), Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error("ACCESS_LEASE_NOT_FOUND", "No owned grant intent exists.")
        })?;
        if member.phase != MembershipPhase::GrantPending {
            return Err(access_error(
                "ACCESS_PHASE_CONFLICT",
                "The grant intent is not pending.",
            ));
        }
        member.native_write_confirmed = true;
        self.checkpoint()
    }

    /// Fresh readback established that a failed grant left no membership.
    /// Keep the failed lease as evidence, but do not retain a cleanup obligation.
    pub(crate) fn confirm_no_grant(&mut self, group: &str) -> Result<(), Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error("ACCESS_LEASE_NOT_FOUND", "No owned grant intent exists.")
        })?;
        if member.phase != MembershipPhase::GrantPending {
            return Err(access_error(
                "ACCESS_PHASE_CONFLICT",
                "The grant intent is not pending.",
            ));
        }
        member.phase = MembershipPhase::Released;
        member.holders.clear();
        for id in &member.lease_ids {
            if let Some(lease) = self.registry.leases.get_mut(id) {
                lease.state = "notGranted".into();
            }
        }
        self.checkpoint()
    }
    pub fn confirm_grant(&mut self, group: &str, revision: &str) -> Result<(), Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error("ACCESS_LEASE_NOT_FOUND", "No owned grant intent exists.")
        })?;
        if member.phase != MembershipPhase::GrantPending {
            return Err(access_error(
                "ACCESS_PHASE_CONFLICT",
                "Only a pending grant can receive Epicor commit confirmation.",
            ));
        }
        member.phase = MembershipPhase::Active;
        member.membership_added = true;
        member.grant_revision = Some(revision.into());
        for id in &member.lease_ids {
            if let Some(lease) = self.registry.leases.get_mut(id) {
                lease.membership_added = true;
                lease.state = "active".into();
            }
        }
        self.checkpoint()
    }
    pub fn release(&mut self, run_id: &str, group: &str) -> Result<bool, Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error(
                "ACCESS_LEASE_NOT_FOUND",
                "No membership lease exists for cleanup.",
            )
        })?;
        if member.phase != MembershipPhase::Active {
            return Err(access_error(
                "ACCESS_RECONCILIATION_REQUIRED",
                "Resolve the interrupted intent before issuing another change in Epicor.",
            ));
        }
        if !member.holders.remove(run_id) {
            return Err(access_error(
                "ACCESS_NOT_OWNED",
                "This run does not hold the membership lease. Cleanup is refused.",
            ));
        }
        for id in &member.lease_ids {
            if let Some(lease) = self.registry.leases.get_mut(id) {
                if lease.holder == run_id {
                    lease.state = "released".into();
                }
            }
        }
        if !member.holders.is_empty() {
            self.checkpoint()?;
            return Ok(false);
        }
        let cleanup = member.membership_added && !member.existed_before;
        member.phase = if cleanup {
            MembershipPhase::CleanupPending
        } else {
            MembershipPhase::Released
        };
        for id in &member.lease_ids {
            if let Some(lease) = self.registry.leases.get_mut(id) {
                if lease.holder == run_id {
                    lease.state = if cleanup {
                        "cleanupPending"
                    } else {
                        "released"
                    }
                    .into();
                }
            }
        }
        self.checkpoint()?;
        Ok(cleanup)
    }
    pub fn confirm_cleanup(&mut self, group: &str) -> Result<(), Diagnostic> {
        let member = self.registry.memberships.get_mut(group).ok_or_else(|| {
            access_error("ACCESS_LEASE_NOT_FOUND", "No owned cleanup intent exists.")
        })?;
        if member.phase != MembershipPhase::CleanupPending {
            return Err(access_error(
                "ACCESS_PHASE_CONFLICT",
                "Only a pending cleanup can receive Epicor readback confirmation.",
            ));
        }
        member.phase = MembershipPhase::Released;
        for id in &member.lease_ids {
            if let Some(lease) = self.registry.leases.get_mut(id) {
                lease.state = "released".into();
            }
        }
        self.checkpoint()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessLease {
    pub schema_version: u32,
    pub lease_id: String,
    pub endpoint: String,
    pub company: String,
    pub caller: String,
    #[serde(default)]
    pub holder: String,
    pub artifact: Identity,
    pub group: String,
    pub native_policy: String,
    pub state: String,
    pub existed_before: bool,
    pub membership_added: bool,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub(crate) authorization: Option<crate::server_changes::AuthorizationRef>,
}
pub fn append_owned_group(current: &str, group: &str) -> Result<(String, bool), Diagnostic> {
    if group.is_empty() || group.contains('~') {
        return Err(Diagnostic::new(
            "INVALID_NATIVE_GROUP",
            "The group must be one exact identity representable by the Epicor tilde-delimited GroupList contract.",
        ));
    }
    if current.split('~').any(|item| item == group) {
        return Ok((current.into(), false));
    }
    Ok((
        if current.is_empty() {
            group.into()
        } else {
            format!("{current}~{group}")
        },
        true,
    ))
}
pub fn remove_owned_group(
    current: &str,
    group: &str,
    owned_addition: bool,
    other_live_leases: bool,
) -> String {
    if !owned_addition || other_live_leases {
        return current.into();
    }
    current
        .split('~')
        .filter(|entry| *entry != group)
        .collect::<Vec<_>>()
        .join("~")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn lease(id: &str) -> AccessLease {
        AccessLease {
            schema_version: 1,
            lease_id: id.into(),
            endpoint: "https://example.test/".into(),
            company: "C".into(),
            caller: "User".into(),
            holder: String::new(),
            artifact: Identity {
                category: "functions".into(),
                kind: "EfxLibrary".into(),
                scope: herbert_contract::Scope::Global,
                keys: BTreeMap::from([("LibraryID".into(), "L".into())]),
            },
            group: "G".into(),
            native_policy: "Ecf.EfxLibrary.SharedWithGroup".into(),
            state: "planned".into(),
            existed_before: false,
            membership_added: false,
            diagnostics: vec![],
            authorization: None,
        }
    }
    #[test]
    fn durable_intents_block_replay_and_shared_holders_delay_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let mut store = LockedRegistry::open(root.path(), "https://example.test/", "User").unwrap();
        assert!(store.request("run1", lease("one"), "A", "100").unwrap());
        assert!(LockedRegistry::open(root.path(), "https://example.test/", "User").is_err());
        drop(store);
        let mut store = LockedRegistry::open(root.path(), "https://example.test/", "User").unwrap();
        assert_eq!(
            store
                .request("run2", lease("two"), "A~G", "101")
                .err()
                .unwrap()
                .code,
            "ACCESS_RECONCILIATION_REQUIRED"
        );
        store.confirm_grant("G", "101").unwrap();
        assert!(!store.request("run2", lease("two"), "A~G", "101").unwrap());
        assert!(!store.release("run1", "G").unwrap());
        assert!(store.release("run2", "G").unwrap());
        drop(store);
        let mut store = LockedRegistry::open(root.path(), "https://example.test/", "User").unwrap();
        assert_eq!(
            store.registry.memberships["G"].phase,
            MembershipPhase::CleanupPending
        );
        store.confirm_cleanup("G").unwrap();
        assert_eq!(
            store.registry.memberships["G"].phase,
            MembershipPhase::Released
        );
        assert!(
            store
                .registry
                .leases
                .values()
                .all(|l| l.state == "released")
        );
    }
    #[test]
    fn cleanup_preserves_preexisting_memberships_and_other_runs() {
        let (existing, added) = append_owned_group("A~B", "B").unwrap();
        assert!(!added);
        assert_eq!(remove_owned_group(&existing, "B", added, false), "A~B");
        let (with, added) = append_owned_group("A~B", "C").unwrap();
        assert!(added);
        assert_eq!(with, "A~B~C");
        assert_eq!(remove_owned_group("A~B~C~D", "C", added, false), "A~B~D");
        assert_eq!(remove_owned_group("A~B~C~D", "C", added, true), "A~B~C~D");
    }
}
