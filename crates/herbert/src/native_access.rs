//! Native account projection for temporary group access.
//! No credential or complete user record belongs in the durable access registry.
use crate::{
    access::{AccessLease, LockedRegistry, MembershipPhase},
    access_store::AccessStore,
    connection::Connection,
};
use herbert_contract::{Diagnostic, Identity};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;

pub(crate) struct CallerAccess {
    pub caller: String,
    pub groups: String,
    pub row_id: String,
    pub revision: Value,
    pub password_change_request: Value,
    safeguards: BTreeMap<String, Value>,
}

impl CallerAccess {
    pub(crate) fn from_native(caller: &str, response: &Value) -> Result<Self, Diagnostic> {
        let rows = response
            .get("returnObj")
            .or_else(|| response.get("ds"))
            .unwrap_or(response)
            .get("UserFile")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() == 1)
            .ok_or_else(|| unavailable("The native read did not return one exact account."))?;
        let row = &rows[0];
        let actual = row["UserID"]
            .as_str()
            .filter(|id| *id == caller)
            .ok_or_else(|| unavailable("The native account does not match the admitted caller."))?;
        if row["SecurityMgr"].as_bool() != Some(true)
            || row["UserDisabled"].as_bool() != Some(false)
            || row["LockedOut"].as_bool() != Some(false)
        {
            return Err(unavailable(
                "The caller is not an enabled Security Manager.",
            ));
        }
        let text = |field: &str| {
            row[field]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(String::from)
                .ok_or_else(|| unavailable("The native account identity is incomplete."))
        };
        let groups = row["GroupList"]
            .as_str()
            .ok_or_else(|| unavailable("The native account omitted its group membership."))?;
        let revision = row
            .get("SysRevID")
            .filter(|value| value.is_number() || value.is_string())
            .ok_or_else(|| unavailable("The native account omitted its revision."))?;
        let password_change_request = row
            .get("PwdChangeRequestOn")
            .ok_or_else(|| unavailable("The account's password-change request is unknown."))?;
        let safeguards = [
            "SecurityMgr",
            "UserDisabled",
            "LockedOut",
            "PwdChangeRequestOn",
            "PwdCreateDate",
            "PwdLastChanged",
            "PwdChangeRequest",
            "PasswordNeverExpires",
            "RequireSso",
            "ExternalIdentity",
            "AllowWeb",
            "DomainName",
        ]
        .into_iter()
        .filter_map(|key| row.get(key).map(|value| (key.into(), value.clone())))
        .collect();
        Ok(Self {
            caller: actual.into(),
            groups: groups.into(),
            row_id: text("SysRowID")?,
            revision: revision.clone(),
            password_change_request: password_change_request.clone(),
            safeguards,
        })
    }

    pub(crate) fn revision_text(&self) -> String {
        self.revision
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| self.revision.to_string())
    }

    fn safeguards_sha256(&self) -> String {
        crate::storage::sha256(
            &serde_json::to_vec(&self.safeguards)
                .expect("Account projection serialization is infallible"),
        )
    }

    pub(crate) fn verify_preserved(&self, after: &Self) -> Result<(), Diagnostic> {
        if self.caller != after.caller || self.row_id != after.row_id {
            return Err(unavailable(
                "Account readback returned a different native identity.",
            ));
        }
        if self.safeguards != after.safeguards {
            return Err(Diagnostic::new(
                "ACCESS_ACCOUNT_FIELDS_CHANGED",
                "Account fields changed during the membership operation. Preserve the ownership record and inspect the native account before another write.",
            ));
        }
        Ok(())
    }
}

/// The caller supplies a remedy read from native policy. A plugin cannot obtain
/// arbitrary group access by putting a group name in its own descriptor.
pub(crate) struct Requirement {
    pub group: String,
    pub native_policy: String,
    pub artifact: Identity,
}

pub(crate) async fn grant(
    root: &AccessStore,
    connection: &Connection,
    holder: &str,
    requirement: Requirement,
    cancel: &CancellationToken,
) -> Result<AccessLease, Diagnostic> {
    let caller = connection
        .caller()
        .await
        .ok_or_else(|| unavailable("Verify the caller before requesting access."))?;
    let mut registry = access_registry(root, connection, &caller, cancel).await?;
    // Read after taking the account lock. A queued operation must use the latest
    // native membership and revision rather than a stale pre-lock projection.
    let before = connection.caller_access(cancel).await?;
    if before.caller != caller {
        return Err(unavailable("The account changed while access was queued."));
    }
    let current_groups = canonical_memberships(connection, &before.groups, cancel).await?;
    let lease = AccessLease {
        schema_version: 1,
        lease_id: uuid::Uuid::new_v4().to_string(),
        endpoint: connection.endpoint.to_string(),
        company: connection.company.clone(),
        caller: before.caller.clone(),
        holder: holder.into(),
        artifact: requirement.artifact,
        group: requirement.group.clone(),
        native_policy: requirement.native_policy,
        state: "requested".into(),
        existed_before: false,
        membership_added: false,
        diagnostics: vec![],
    };
    let needs_grant = registry.request(
        holder,
        lease.clone(),
        &current_groups,
        &before.revision_text(),
    )?;
    if needs_grant {
        registry.record_account_proof(&lease.group, &before.row_id, &before.safeguards_sha256())?;
        let result = connection.add_own_group(&lease.group, cancel).await;
        if result.is_ok() {
            // Persist a successful native response before the separate readback.
            // A crash after an uncertain transport result remains uncertain.
            registry.record_grant_response(&lease.group)?;
        }
        let after = connection.caller_access(&CancellationToken::new()).await?;
        before.verify_preserved(&after)?;
        let after_groups =
            canonical_memberships(connection, &after.groups, &CancellationToken::new()).await?;
        let (_, missing) = crate::access::append_owned_group(&after_groups, &lease.group)?;
        if missing {
            registry.confirm_no_grant(&lease.group)?;
            return Err(result.err().unwrap_or_else(|| {
                Diagnostic::new(
                    "ACCESS_GRANT_UNVERIFIED",
                    "Native readback did not confirm the required membership.",
                )
            }));
        }
        if let Err(error) = result {
            return Err(Diagnostic::new(
                "ACCESS_GRANT_UNCERTAIN",
                format!(
                    "Membership is present after an uncertain write ({}). Keep the grant intent; do not remove or replay it without ownership proof.",
                    error.code
                ),
            ));
        }
        registry.confirm_grant(&lease.group, &after.revision_text())?;
    }
    let saved = registry.registry.leases[&lease.lease_id].clone();
    drop(registry);
    if needs_grant {
        connection.refresh_access(cancel).await?;
    }
    Ok(saved)
}

/// A borrower names an existing host grant and the exact run scope. These are
/// checked again from durable authority immediately before each holder change.
/// A preference or generic temporary-access flag cannot supply group authority.
pub(crate) struct BorrowAuthority<'a> {
    pub grants_root: &'a std::path::Path,
    pub grant_id: &'a str,
    pub scope: &'a crate::server_changes::MaterialScope,
    pub use_: crate::server_changes::Use<'a>,
    pub temporary_access: bool,
}
impl BorrowAuthority<'_> {
    fn permits(&self,group:&str,intent:&str)->Result<(),Diagnostic> {
        if !self.temporary_access {return Err(unavailable("Temporary access is off; this run cannot extend another run’s membership."));}
        let grant=crate::server_changes::get(self.grants_root,self.grant_id)?;
        if !grant.permits(self.scope,&self.use_) {return Err(unavailable("The selected access authorization is absent, changed, expired or revoked."));}
        if !self.scope.capabilities.get("$native-access").is_some_and(|c|c.effects.values().any(|effect|effect["kind"]=="borrowOwnedMembership"&&effect["group"]==group&&effect["ownershipIntent"]==intent)) {
            return Err(unavailable("This group and ownership generation are outside the selected policy-supported borrow authority."));
        }
        Ok(())
    }
    fn check_member(&self,group:&str,member:&crate::access::Membership,present:bool)->Result<(),Diagnostic> {
        if member.phase!=MembershipPhase::Active || !member.membership_added || member.existed_before || !present {
            return Err(unavailable("The original owned membership is no longer active. Obtain a fresh preview; borrowing cannot recreate it."));
        }
        self.permits(group,&member.intent_id)
    }
    fn selected_groups(&self)->Vec<String> {
        self.scope.capabilities.get("$native-access").into_iter().flat_map(|c|c.effects.values()).filter(|v|v["kind"]=="borrowOwnedMembership").filter_map(|v|v["group"].as_str().map(String::from)).collect::<std::collections::BTreeSet<_>>().into_iter().collect()
    }
}
/// Borrow only specifically reviewed policy-supported groups. Revocation and
/// ownership are rechecked under the account lock for every new holder. This
/// never converts a vanished/released original membership into a new grant.
pub(crate) async fn borrow_active(
    root: &AccessStore,
    connection: &Connection,
    holder: &str,
    authority: &BorrowAuthority<'_>,
    cancel: &CancellationToken,
) -> Result<bool, Diagnostic> {
    if !authority.temporary_access {return Err(unavailable("Temporary access is off; no membership can be borrowed."));}
    let caller=connection.caller().await.ok_or_else(||unavailable("Verify the caller before borrowing access."))?;
    if authority.scope.endpoint!=connection.endpoint.as_str() || authority.scope.company!=connection.company || authority.scope.principal!=caller {return Err(unavailable("The borrow authority belongs to a different target or caller."));}
    let selected=authority.selected_groups();
    if selected.is_empty(){return Ok(false);}
    // Validate before even entering the mutating local coordination route.
    for group in &selected {
        let snapshot=root.snapshot(connection.endpoint.as_str(),&caller)?;
        let member=snapshot.memberships.get(group).ok_or_else(||unavailable("The original owned membership no longer exists. Obtain a fresh preview."))?;
        authority.permits(group,&member.intent_id)?;
    }
    let mut registry=access_registry(root,connection,&caller,cancel).await?;
    let before=connection.caller_access(cancel).await?;
    let current=canonical_memberships(connection,&before.groups,cancel).await?;
    let mut changed=false;
    for group in selected {
        let member=registry.registry.memberships.get(&group).ok_or_else(||unavailable("The original owned membership disappeared before dispatch."))?;
        authority.check_member(&group,member,current.split('~').any(|g|g==group))?;
        if member.holders.contains(holder){continue;}
        let original=member.lease_ids.iter().find_map(|id|registry.registry.leases.get(id)).ok_or_else(||unavailable("The owned membership has no original lease."))?;
        let mut lease=original.clone();lease.lease_id=uuid::Uuid::new_v4().to_string();lease.holder=holder.into();lease.company=connection.company.clone();
        lease.artifact=Identity{category:"NativeOperation".into(),kind:"AccessDependency".into(),scope:herbert_contract::Scope::Company{company:connection.company.clone()},keys:BTreeMap::from([("OperationID".into(),holder.into())])};
        if registry.request(holder,lease,&current,&before.revision_text())? {return Err(unavailable("Borrowing access must not require a new membership."));}
        changed=true;
    }
    drop(registry);
    if changed {connection.refresh_access(cancel).await?;}
    Ok(changed)
}

/// Remove only confirmed, owned additions when this holder releases its last
/// claim. Fresh GroupList readback preserves unrelated later memberships.
pub(crate) async fn release(
    root: &AccessStore,
    connection: &Connection,
    holder: &str,
    cancel: &CancellationToken,
) -> Result<Vec<AccessLease>, Diagnostic> {
    let caller = connection.caller().await.ok_or_else(|| {
        Diagnostic::new(
            "ADMISSION_REQUIRED",
            "Reconnect as the original caller to restore access.",
        )
    })?;
    let mut registry = access_registry(root, connection, &caller, cancel).await?;
    if registry
        .registry
        .leases
        .values()
        .any(|lease| lease.holder == holder && lease.company != connection.company)
    {
        return Err(Diagnostic::new(
            "ACCESS_SCOPE_MISMATCH",
            "Reconnect to the original access operation company before cleanup.",
        ));
    }
    let groups = registry
        .registry
        .memberships
        .iter()
        .filter(|(_, member)| member.holders.contains(holder))
        .map(|(group, _)| group.clone())
        .collect::<Vec<_>>();
    let mut changed = false;
    for group in groups {
        let member = registry.registry.memberships[&group].clone();
        if member.phase == MembershipPhase::GrantPending {
            let current = connection.caller_access(cancel).await?;
            let groups = canonical_memberships(connection, &current.groups, cancel).await?;
            let (_, missing) = crate::access::append_owned_group(&groups, &group)?;
            if missing {
                registry.confirm_no_grant(&group)?;
                continue;
            }
            if !member.native_write_confirmed
                || member.account_row_id.as_deref() != Some(current.row_id.as_str())
                || member.account_safeguards_sha256.as_deref()
                    != Some(current.safeguards_sha256().as_str())
            {
                return Err(Diagnostic::new(
                    "ACCESS_GRANT_UNCERTAIN",
                    "An interrupted grant has no sufficient ownership readback. Preserve it without another account write.",
                ));
            }
            registry.confirm_grant(&group, &current.revision_text())?;
        }
        if registry.release(holder, &group)? {
            restore_group(connection, &mut registry, &group, cancel).await?;
            changed = true;
        }
    }
    // A previous cleanup can have removed its final holder before the host
    // stopped. Its exact confirmed addition remains safe to reconcile here.
    let pending = registry
        .registry
        .memberships
        .iter()
        .filter(|(_, member)| {
            member.phase == MembershipPhase::CleanupPending
                && member.lease_ids.iter().any(|id| {
                    registry
                        .registry
                        .leases
                        .get(id)
                        .is_some_and(|lease| lease.holder == holder)
                })
        })
        .map(|(group, _)| group.clone())
        .collect::<Vec<_>>();
    for group in pending {
        restore_group(connection, &mut registry, &group, cancel).await?;
        changed = true;
    }
    let leases = registry
        .registry
        .leases
        .values()
        .filter(|lease| lease.holder == holder)
        .cloned()
        .collect();
    drop(registry);
    if changed {
        connection.refresh_access(cancel).await?;
    }
    Ok(leases)
}

async fn restore_group(
    connection: &Connection,
    registry: &mut LockedRegistry,
    group: &str,
    cancel: &CancellationToken,
) -> Result<(), Diagnostic> {
    let member = &registry.registry.memberships[group];
    if member.phase != MembershipPhase::CleanupPending
        || !member.membership_added
        || member.existed_before
        || !member.holders.is_empty()
    {
        return Err(Diagnostic::new(
            "ACCESS_NOT_OWNED",
            "The membership has no exclusive owned cleanup intent.",
        ));
    }
    let before = connection.caller_access(cancel).await?;
    if member.account_row_id.as_deref() != Some(before.row_id.as_str()) {
        return Err(unavailable("The account identity changed before cleanup."));
    }
    let mut retained = Vec::new();
    for entry in before.groups.split('~') {
        if entry.is_empty() || connection.canonical_group(entry, cancel).await? != group {
            retained.push(entry);
        }
    }
    let restored = retained.join("~");
    if restored != before.groups {
        let result = connection
            .restore_own_groups(&before, &restored, cancel)
            .await;
        let after = connection.caller_access(&CancellationToken::new()).await?;
        before.verify_preserved(&after)?;
        let after_groups =
            canonical_memberships(connection, &after.groups, &CancellationToken::new()).await?;
        if after_groups.split('~').any(|entry| entry == group) {
            return Err(result.err().unwrap_or_else(|| {
                Diagnostic::new(
                    "ACCESS_CLEANUP_UNVERIFIED",
                    "Native readback still contains the owned membership.",
                )
            }));
        }
    }
    registry.confirm_cleanup(group)
}

pub(crate) fn verify_update_result(response: &Value) -> Result<(), Diagnostic> {
    let result = response.get("returnObj").unwrap_or(response);
    let errors = result
        .get("BOUpdError")
        .or_else(|| result.get("Errors"))
        .and_then(Value::as_array);
    if errors.is_some_and(|rows| !rows.is_empty()) {
        return Err(Diagnostic::new(
            "ACCESS_UPDATE_REFUSED",
            "The native membership update returned errors. Readback must establish its actual effect.",
        ));
    }
    Ok(())
}

fn unavailable(message: &str) -> Diagnostic {
    Diagnostic::new("ACCESS_ACCOUNT_UNVERIFIED", message)
}

async fn canonical_memberships(
    connection: &Connection,
    groups: &str,
    cancel: &CancellationToken,
) -> Result<String, Diagnostic> {
    let mut canonical = Vec::new();
    for group in groups.split('~').filter(|group| !group.is_empty()) {
        let group = connection.canonical_group(group, cancel).await?;
        if !canonical.contains(&group) {
            canonical.push(group);
        }
    }
    Ok(canonical.join("~"))
}
async fn access_registry(
    root: &AccessStore,
    connection: &Connection,
    caller: &str,
    cancel: &CancellationToken,
) -> Result<LockedRegistry, Diagnostic> {
    let root = root.clone();
    let endpoint = connection.endpoint.to_string();
    let caller = caller.to_string();
    // Cancellation can stop waiting. The blocking task only acquires a file
    // lock. It has no native mutation and drops the lock if its result is unused.
    tokio::select! {
        _ = cancel.cancelled() => Err(Diagnostic::new("CANCELED", "Access was canceled while waiting for its account lock.")),
        result = tokio::task::spawn_blocking(move || root.wait(&endpoint, &caller)) => {
            result.map_err(|_| unavailable("The access lock task failed."))?
        }
    }
}

#[cfg(test)]
mod borrow_authority_tests {
    use super::*;
    use crate::server_changes::{self,Capability,Lifetime,MaterialScope,Use};
    use serde_json::json;
    #[test]
    fn borrow_requires_exact_selected_policy_group_live_grant_and_original_ownership_generation() {
        let root=tempfile::tempdir().unwrap();
        let scope=MaterialScope{endpoint:"https://fixture.invalid/".into(),company:"C".into(),principal:"manager".into(),capabilities:BTreeMap::from([("$native-access".into(),Capability{binding:"stored-native-policy-remedy-v1".into(),selection:BTreeMap::new(),effects:BTreeMap::from([("access:Ice.BO.TestSvc/GetRows/ELIGIBLE".into(),json!({"kind":"borrowOwnedMembership","service":"Ice.BO.TestSvc","method":"GetRows","group":"ELIGIBLE","ownershipIntent":"original-intent","policy":{"SecCode":"bo.Test"}}))])})])};
        let grant=server_changes::create(root.path(),scope.clone(),Lifetime::OneRun{request_id:"request".into()},None,"explicitLocalClient").unwrap();
        let mut authority=BorrowAuthority{grants_root:root.path(),grant_id:&grant.id,scope:&scope,use_:Use{request_id:"request",session_id:"session",workflow_id:None},temporary_access:true};
        assert_eq!(authority.selected_groups(),["ELIGIBLE"]);assert!(authority.permits("ELIGIBLE","original-intent").is_ok());
        assert!(authority.permits("UNRELATED-ACTIVE","original-intent").is_err());assert!(authority.permits("ELIGIBLE","replacement-owner-after-release").is_err());
        let mut member=crate::access::Membership{group:"ELIGIBLE".into(),holders:std::collections::BTreeSet::from(["original-holder".into()]),phase:MembershipPhase::Active,existed_before:false,membership_added:true,before_revision:"1".into(),grant_revision:Some("2".into()),intent_id:"original-intent".into(),lease_ids:Default::default(),native_write_confirmed:true,account_row_id:Some("account".into()),account_safeguards_sha256:Some("proof".into())};
        assert!(authority.check_member("ELIGIBLE",&member,true).is_ok());assert!(authority.check_member("ELIGIBLE",&member,false).is_err());
        member.phase=MembershipPhase::Released;member.holders.clear();assert!(authority.check_member("ELIGIBLE",&member,true).is_err()); // same original intent, released between preview and start
        authority.temporary_access=false;assert!(authority.permits("ELIGIBLE","original-intent").is_err());authority.temporary_access=true;
        server_changes::revoke(root.path(),&grant.id).unwrap();assert!(authority.permits("ELIGIBLE","original-intent").is_err());
        assert!(server_changes::get(root.path(),&grant.id).unwrap().permits_owned_cleanup(&scope));
    }
}
