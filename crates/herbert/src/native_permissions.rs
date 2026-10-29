//! Apply native-policy group remedies through the same durable ownership path.
//! A plugin's requested method is still checked before this adapter is called.
use super::*;
use super::server_change_dispatch::ServerChangeRefusal;

/// One per run, group and ownership generation: read through a membership this
/// run is not authorized to hold.
const UNHELD_MEMBERSHIP: &str = "ACCESS_MEMBERSHIP_NOT_HELD";
/// A read relied on an unheld membership that was released before it finished.
const MEMBERSHIP_LOST_DURING_READ: &str = "ACCESS_MEMBERSHIP_LOST_DURING_READ";

impl Host {
    /// Inspect possible stored-policy remedies without taking/borrowing a lease,
    /// refreshing access, invoking a helper or retrying a refused collection read.
    pub(super) async fn preview_policy_effects(
        &self,
        connection: &Arc<Connection>,
        selected: &BTreeMap<String, std::collections::BTreeSet<String>>,
        cancel: &CancellationToken,
    ) -> Result<BTreeMap<String, Value>, Diagnostic> {
        let before = connection.caller_access(cancel).await?;
        let ownership=self.access_store.snapshot(connection.endpoint.as_str(),&before.caller)?;
        let owned=ownership.memberships.iter().filter(|(_,m)|m.membership_added&&!m.existed_before&&m.phase==crate::access::MembershipPhase::Active).map(|(g,m)|(g.clone(),m.intent_id.clone())).collect::<BTreeMap<_,_>>();
        let mut policies = Vec::new();
        let mut group_names = before.groups.split('~').filter(|v| !v.is_empty()).map(String::from).collect::<std::collections::BTreeSet<_>>();
        for (service, methods) in selected {
            let Some(code) = native_security_code(service) else { continue; };
            let response = match connection.security_read("Ice.BO.ObjectSecuritySvc", "GetByIDEx",
                json!({"secCode":code,"pcComp":connection.company,"strictPrimKey":false}), cancel).await {
                Ok(response) => response,
                Err(error) if native_policy_absent(&error) => continue,
                Err(error) => return Err(error),
            };
            for method in methods {
                if let Some(policy) = controlling_policy(&response.value, &code, method, &connection.company)? {
                    for field in ["EntryList", "NoEntryList"] {
                        for group in policy_ids(policy[field].as_str().ok_or_else(policy_invalid)?) {
                            if group != "*" { group_names.insert(group.into()); }
                        }
                    }
                    policies.push((service.clone(),method.clone(),policy.clone()));
                }
            }
        }
        let mut canonical = BTreeMap::new();
        for group in group_names { canonical.insert(group.clone(),connection.canonical_group(&group,cancel).await?); }
        let current = before.groups.split('~').filter(|v| !v.is_empty()).filter_map(|v|canonical.get(v).cloned()).collect::<std::collections::BTreeSet<_>>();
        for group in &current {
            if ownership.memberships.get(group).is_some_and(|m|!m.existed_before&&(m.membership_added||m.phase==crate::access::MembershipPhase::GrantPending)&&m.phase!=crate::access::MembershipPhase::Active) {
                return Err(Diagnostic::new("ACCESS_OWNERSHIP_UNCERTAIN","A current temporary membership has unresolved ownership. Preview preserved it without borrowing or reconciliation."));
            }
        }
        policy_access_effects(policies,&canonical,&current,&owned)
    }

    pub(super) async fn native_read_with_access(
        &self,
        connection: &Arc<Connection>,
        service: &str,
        method: &str,
        arguments: Value,
        export: bool,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        holder: &str,
        temporary_access: bool,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let result=self.native_read_with_access_inner(connection,service,method,arguments,export,run,identity,holder,temporary_access,cancel).await;
        let Some(run)=run else {return result;};
        match result {
            Err(error) if authorization_refusal(&error)||error.code=="SERVER_CHANGE_AUTHORIZATION_REQUIRED" => Err(self.attribute_unheld_reliance(run,service,method,error).await),
            other=>other,
        }
    }

    /// A refused read may follow another run's release of a membership this run
    /// relied on without holding. Name that reliance on the failure so the item
    /// stays failed/partial with its cause, and re-preview this method next time.
    async fn attribute_unheld_reliance(&self,run:&Arc<Run>,service:&str,method:&str,mut error:Diagnostic)->Diagnostic {
        if let Ok(journal)=collection::snapshot(run).await {
            let prefix=format!("{service}\u{1f}{method}\u{1f}");
            if let Ok(mut memo)=self.borrow_preview_negative.lock() {
                if let Some(keys)=memo.get_mut(&journal.run_id) {keys.retain(|key|!key.starts_with(&prefix));}
            }
            // Only reliance recorded for the refused service/method is attributed.
            let relied=journal.diagnostics.iter().filter(|d|d.code==UNHELD_MEMBERSHIP&&d.context.get("service").and_then(Value::as_str)==Some(service)&&d.context.get("method").and_then(Value::as_str)==Some(method)).map(|d|json!({"group":d.context.get("group"),"ownershipIntent":d.context.get("ownershipIntent")})).collect::<Vec<_>>();
            if !relied.is_empty() {error.context.insert("unheldMemberships".into(),Value::Array(relied));}
        }
        error
    }

    async fn native_read_with_access_inner(
        &self,
        connection: &Arc<Connection>,
        service: &str,
        method: &str,
        arguments: Value,
        export: bool,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        holder: &str,
        temporary_access: bool,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let relied=self.borrow_method_access(connection,service,method,run,temporary_access,cancel).await?;
        let read = connection.native_read(service, method, arguments.clone(), export, cancel);
        let result = if let Some(run) = run {
            crate::activity::TRANSPORT_LOG.scope(
                (run.activity.clone(), service.into(), method.into()), read).await
        } else { read.await };
        let Err(refusal) = result else {
            // Rows returned through an unheld membership are complete only if that
            // membership was still in place when the read finished. An empty or
            // short page after its release must not look complete.
            if let (Some(run),false)=(run,relied.is_empty()) {self.confirm_unheld_reliance(connection,run,service,method,&relied)?;}
            return result;
        };
        if !temporary_access || !authorization_refusal(&refusal) {
            return Err(refusal);
        }
        // A stored method/object policy is authority. HTTP status, plugin hints
        // and a similarly named group are not authority to change an account.
        if !self
            .authorize_native_policy(connection, service, method, holder, run, identity, cancel)
            .await?
        {
            return Err(refusal);
        }
        let read = connection.native_read(service, method, arguments, export, cancel);
        if let Some(run) = run {
            crate::activity::TRANSPORT_LOG.scope(
                (run.activity.clone(), service.into(), method.into()), read).await
        } else { read.await }
    }

    /// Returns the (group, ownership intent) memberships this call reads through
    /// without holding them, so the read can confirm they were still in place.
    async fn borrow_method_access(&self,connection:&Arc<Connection>,service:&str,method:&str,run:Option<&Arc<Run>>,temporary_access:bool,cancel:&CancellationToken)->Result<Vec<(String,String)>,Diagnostic> {
        if !temporary_access {return Ok(vec![]);}
        let Some(run)=run else {return Ok(vec![]);};
        let journal=collection::snapshot(run).await?;
        if !journal.temporary_access_enabled {return Ok(vec![]);}
        // The registry is keyed by the run's caller. A connection for another
        // target, company or caller would otherwise see no ownership and read
        // unattributed; refuse before the read instead.
        if connection.endpoint.as_str()!=journal.endpoint||connection.company!=journal.company||connection.caller().await.as_deref()!=Some(journal.caller.as_str()) {
            return Err(Diagnostic::new("ACCESS_SCOPE_MISMATCH","The run and connection do not share the same access owner."));
        }
        // Fresh on every call: the memo key below is derived from this snapshot.
        let ownership=self.access_store.snapshot(connection.endpoint.as_str(),&journal.caller)?;
        let unheld=ownership.memberships.iter().filter(|(_,member)|member.phase==crate::access::MembershipPhase::Active&&member.membership_added&&!member.existed_before&&!member.holders.contains(&journal.run_id)).map(|(group,member)|(group.clone(),(member.intent_id.clone(),member.phase.clone()))).collect::<BTreeMap<_,_>>();
        if unheld.is_empty(){return Ok(vec![]);}
        // Only "this method's policy offers no borrow for exactly these unheld
        // memberships" is reused. The key covers every owned registry group for
        // the account, Active or not, so any phase or generation change misses.
        // While any owned group is not Active (grant or cleanup pending,
        // uncertain, released but possibly still present), nothing is read from
        // or written to the memo: preview must run its ownership-uncertainty check.
        let owned=ownership.memberships.iter().filter(|(_,member)|!member.existed_before&&(member.membership_added||member.phase==crate::access::MembershipPhase::GrantPending)).map(|(group,member)|(group.clone(),(member.intent_id.clone(),member.phase.clone()))).collect::<BTreeMap<_,_>>();
        let memo_usable=owned.values().all(|(_,phase)|*phase==crate::access::MembershipPhase::Active);
        let memo_key=format!("{service}\u{1f}{method}\u{1f}{}",serde_json::to_string(&owned).map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Access state is unavailable."))?);
        if memo_usable&&self.borrow_preview_negative.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Access state is unavailable."))?.get(&journal.run_id).is_some_and(|keys|keys.contains(&memo_key)) {return Ok(vec![]);}
        let methods=BTreeMap::from([(service.into(),std::collections::BTreeSet::from([method.into()]))]);
        let effects=self.preview_policy_effects(connection,&methods,cancel).await?;
        let candidates=effects.iter().filter(|(_,effect)|effect["kind"]=="borrowOwnedMembership"&&effect["group"].as_str().is_some_and(|group|unheld.contains_key(group))).collect::<Vec<_>>();
        if candidates.is_empty() {
            if memo_usable {self.borrow_preview_negative.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Access state is unavailable."))?.entry(journal.run_id.clone()).or_default().insert(memo_key);}
            return Ok(vec![]);
        }
        let mut selected=std::collections::BTreeSet::new();let mut original=None;let mut relied=Vec::new();
        for (key,effect) in candidates {
            match self.check_server_change(run,connection,"$native-access",key,Some("stored-native-policy-remedy-v1"),effect,None).await {
                Ok(authority)=>{original=Some(authority);selected.insert(key.clone());}
                // Never authorized for this run: read without becoming a holder,
                // exactly as with access off, and record the reliance once per
                // run, group, ownership generation, service and method.
                Err(ServerChangeRefusal::Unauthorized(_))=>{
                    let mut diagnostic=Diagnostic::new(UNHELD_MEMBERSHIP,"This run is reading through another run’s temporary membership that it is not authorized to hold. If that run releases it, affected items stay incomplete.");
                    diagnostic.context.insert("group".into(),effect["group"].clone());
                    diagnostic.context.insert("ownershipIntent".into(),effect["ownershipIntent"].clone());
                    diagnostic.context.insert("service".into(),json!(service));
                    diagnostic.context.insert("method".into(),json!(method));
                    record_diagnostic(run,diagnostic).await?;
                    if let (Some(group),Some(intent))=(effect["group"].as_str(),effect["ownershipIntent"].as_str()) {relied.push((group.to_owned(),intent.to_owned()));}
                }
                Err(ServerChangeRefusal::Denied(error))=>return Err(error),
            }
        }
        let Some(original)=original else {return Ok(relied);};
        let session=self.authorization_session.read().await.clone();
        let authority=crate::native_access::PolicyAuthority{grants_root:&self.state_root,grant_id:&original.grant_id,scope:&original.material_scope,use_:crate::server_changes::Use{request_id:&original.request_id,session_id:&session,workflow_id:original.workflow_id.as_deref()},temporary_access:journal.temporary_access_enabled,selected_effects:selected};
        let result=crate::native_access::borrow_active(&self.access_store,connection,&journal.run_id,&authority,cancel).await;
        self.checkpoint_access(run,connection).await?;
        result.map(|_|relied)
    }

    /// After a read that relied on unheld memberships, re-snapshot the registry.
    /// If any is no longer the same Active ownership generation, the page may be
    /// short or empty: fail it as retryable, attributed, never silently complete.
    fn confirm_unheld_reliance(&self,connection:&Connection,run:&Arc<Run>,service:&str,method:&str,relied:&[(String,String)])->Result<(),Diagnostic> {
        let caller=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?.journal.caller.clone();
        let after=self.access_store.snapshot(connection.endpoint.as_str(),&caller)?;
        let lost=relied.iter().filter(|(group,intent)|!after.memberships.get(group).is_some_and(|member|member.phase==crate::access::MembershipPhase::Active&&member.intent_id==*intent)).map(|(group,intent)|json!({"group":group,"ownershipIntent":intent})).collect::<Vec<_>>();
        if lost.is_empty() {return Ok(());}
        let mut error=Diagnostic::new(MEMBERSHIP_LOST_DURING_READ,"Another run released a temporary membership while this read relied on it. The returned rows may be incomplete; retry this item.");
        error.retryable=true;
        error.context.insert("unheldMemberships".into(),Value::Array(lost));
        error.context.insert("service".into(),json!(service));
        error.context.insert("method".into(),json!(method));
        Err(error)
    }

    /// Negative borrow previews are scoped to one execution of a run.
    pub(super) fn forget_borrow_previews(&self,run_id:&str) {
        if let Ok(mut memo)=self.borrow_preview_negative.lock() {memo.remove(run_id);}
    }

    pub(super) async fn authorize_native_policy(
        &self,
        connection: &Arc<Connection>,
        service: &str,
        method: &str,
        holder: &str,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        cancel: &CancellationToken,
    ) -> Result<bool, Diagnostic> {
        let Some(code) = native_security_code(service) else {
            return Ok(false);
        };
        let response = connection
            .security_read(
                "Ice.BO.ObjectSecuritySvc",
                "GetByIDEx",
                json!({"secCode": code, "pcComp": connection.company, "strictPrimKey": false}),
                cancel,
            )
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) if native_policy_absent(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let Some(policy) = controlling_policy(&response.value, &code, method, &connection.company)?
        else {
            return Ok(false);
        };
        let before = connection.caller_access(cancel).await?;
        let mut current = Vec::new();
        for group in before.groups.split('~').filter(|group| !group.is_empty()) {
            current.push(connection.canonical_group(group, cancel).await?);
        }
        let denied = policy["NoEntryList"].as_str().ok_or_else(policy_invalid)?;
        if policy_ids(denied).any(|group| group == "*")
            || policy["DisallowAll"].as_bool() == Some(true)
        {
            return Ok(false);
        }
        for group in policy_ids(denied) {
            if current.contains(&connection.canonical_group(group, cancel).await?) {
                return Ok(false);
            }
        }
        let allowed = policy["EntryList"].as_str().ok_or_else(policy_invalid)?;
        if policy_ids(allowed).any(|group| group == "*")
            || policy["AllowAll"].as_bool() == Some(true)
        {
            return Ok(false);
        }
        let mut candidates = Vec::new();
        for group in policy_ids(allowed) {
            let group = connection.canonical_group(group, cancel).await?;
            if current.contains(&group) {
                return Ok(false);
            }
            if !candidates.contains(&group) {
                candidates.push(group);
            }
        }
        candidates.sort();
        for group in candidates {
            let mut denied_by_addition = false;
            for excluded in policy_ids(denied) {
                if connection.canonical_group(excluded, cancel).await? == group {
                    denied_by_addition = true;
                }
            }
            if denied_by_addition {
                continue;
            }
            let artifact = identity.cloned().unwrap_or_else(|| Identity {
                category: "NativeOperation".into(),
                kind: "NativeMethod".into(),
                scope: Scope::Company {
                    company: connection.company.clone(),
                },
                keys: BTreeMap::from([
                    ("Service".into(), service.into()),
                    ("Method".into(), method.into()),
                ]),
            });
            let evidence = json!({"service":service,"method":method,"company":policy["Company"],
                "securityCode":policy["SecCode"],"entryList":policy["EntryList"],"noEntryList":policy["NoEntryList"],
                "securityManager":policy["SecurityMgr"],"sysRowId":policy["SysRowID"],"sysRevId":policy["SysRevID"]});
            let Some(run)=run else {return Err(Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED","An Epicor membership change needs an explicitly authorized logical run."));};
            let journal=collection::snapshot(run).await?;
            if holder!=journal.run_id {return Err(Diagnostic::new("ACCESS_SCOPE_MISMATCH","The access holder does not match the authorized logical run."));}
            let key=format!("access:{service}/{method}/{group}");
            let expected=json!({"kind":"groupMembership","service":service,"method":method,"group":group,"ownershipIntent":Value::Null,"policy":policy_material(policy)});
            let original=self.require_server_change(run,connection,"$native-access",&key,Some("stored-native-policy-remedy-v1"),&expected,None).await?;
            let session=self.authorization_session.read().await.clone();
            let authority=crate::native_access::PolicyAuthority{grants_root:&self.state_root,grant_id:&original.grant_id,scope:&original.material_scope,use_:crate::server_changes::Use{request_id:&original.request_id,session_id:&session,workflow_id:original.workflow_id.as_deref()},temporary_access:journal.temporary_access_enabled,selected_effects:std::collections::BTreeSet::from([key])};
            let result = crate::native_access::grant(
                &self.access_store,
                connection,
                holder,
                crate::native_access::Requirement {
                    group,
                    native_policy: evidence.to_string(),
                    artifact,
                },
                &authority,
                cancel,
            )
            .await;
            self.checkpoint_access(run, connection).await?;
            result?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) async fn checkpoint_access(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        let (endpoint, caller, run_id) = {
            let storage=run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?;
            (storage.journal.endpoint.clone(),storage.journal.caller.clone(),storage.journal.run_id.clone())
        };
        if endpoint != connection.endpoint.as_str()
            || connection
                .caller()
                .await
                .is_some_and(|current| current != caller)
        {
            return Err(Diagnostic::new(
                "ACCESS_SCOPE_MISMATCH",
                "The run and connection do not share the same access owner.",
            ));
        }
        // A failed session refresh can clear admission after a grant. The run's
        // fixed original caller still identifies the local ownership evidence.
        let store = self.access_store.clone();
        let endpoint = connection.endpoint.to_string();
        let leases = blocking(move || {
            let registry = store.wait(&endpoint, &caller)?;
            Ok(registry
                .registry
                .leases
                .values()
                .filter(|lease| lease.holder == run_id)
                .cloned()
                .collect::<Vec<_>>())
        })
        .await?;
        let storage = run.storage.clone();
        blocking(move || {
            let mut storage=storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?;
            if storage.journal.access_leases != leases {
                storage.journal.access_leases=leases;
                storage.checkpoint()?;
            }
            Ok(())
        }).await
    }

    pub(super) async fn cleanup_access(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        let journal = collection::snapshot(run).await?;
        if journal
            .helper_libraries
            .iter()
            .any(|lease| lease.phase != "removed")
        {
            return Err(Diagnostic::new(
                "ACCESS_CLEANUP_WAITING_FOR_LIBRARY",
                "Epicor helper removal is pending. Keep the owned access lease for cleanup.",
            ));
        }
        if journal
            .helper_transfers
            .iter()
            .any(|transfer| !matches!(transfer.phase.as_str(), "closed" | "notAdmitted"))
        {
            return Err(Diagnostic::new(
                "ACCESS_CLEANUP_WAITING_FOR_TRANSFERS",
                "Server transfer cleanup is pending. Keep the access lease so the original Epicor user can finish it.",
            ));
        }
        let result = crate::native_access::release(
            &self.access_store,
            connection,
            &journal.run_id,
            &CancellationToken::new(),
        )
        .await;
        self.checkpoint_access(run, connection).await?;
        result.map(|_| ())
    }
}

/// Recheck the stored controlling policy after the account lock is acquired.
/// This is a native read; it never grants or borrows membership.
pub(crate) async fn verify_access_effect(connection:&Connection,effect:&Value,cancel:&CancellationToken)->Result<(),Diagnostic> {
    let service=effect["service"].as_str().ok_or_else(policy_invalid)?;
    let method=effect["method"].as_str().ok_or_else(policy_invalid)?;
    let code=native_security_code(service).ok_or_else(policy_invalid)?;
    let response=connection.security_read("Ice.BO.ObjectSecuritySvc","GetByIDEx",json!({"secCode":code,"pcComp":connection.company,"strictPrimKey":false}),cancel).await?;
    let current=controlling_policy(&response.value,&code,method,&connection.company)?.ok_or_else(||Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED","The stored Epicor policy changed or is no longer controlling. Obtain a fresh preview before changing access."))?;
    if policy_material(current)!=effect["policy"] {return Err(Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED","The controlling Epicor policy changed. Preserve existing ownership and review new authority before changing access."));}
    Ok(())
}

fn policy_access_effects(
    policies:Vec<(String,String,Value)>,
    canonical:&BTreeMap<String,String>,
    current:&std::collections::BTreeSet<String>,
    owned:&BTreeMap<String,String>,
)->Result<BTreeMap<String,Value>,Diagnostic> {
    let mut effects = BTreeMap::new();
        for (service,method,policy) in policies {
            let denied = policy_ids(policy["NoEntryList"].as_str().ok_or_else(policy_invalid)?).collect::<Vec<_>>();
            let allowed = policy_ids(policy["EntryList"].as_str().ok_or_else(policy_invalid)?).collect::<Vec<_>>();
            if denied.contains(&"*") || policy["DisallowAll"] == true
                || denied.iter().filter_map(|g|canonical.get(*g)).any(|g|current.contains(g))
                || allowed.contains(&"*") || policy["AllowAll"] == true
                || allowed.iter().filter_map(|g|canonical.get(*g)).any(|g|current.contains(g)&&!owned.contains_key(g)) { continue; }
            for group in allowed {
                let group = canonical.get(group).ok_or_else(policy_invalid)?;
                if denied.iter().filter_map(|g|canonical.get(*g)).any(|g|g == group) { continue; }
                crate::access::append_owned_group("",group)?;
                effects.insert(format!("access:{service}/{method}/{group}"),
                    json!({"kind":if current.contains(group)&&owned.contains_key(group){"borrowOwnedMembership"}else{"groupMembership"},"service":service,"method":method,"group":group,
                        "ownershipIntent":owned.get(group).filter(|_|current.contains(group)),"policy":policy_material(&policy),
                        "condition":if current.contains(group)&&owned.contains_key(group){"Before a selected policy-supported method uses another run’s owned temporary membership"}else{"Only if Epicor refuses the request because this account lacks access"}}));
            }
        }
        Ok(effects)
}

/// Revision-only churn is not a material permission change. Preserve the exact
/// policy row identity, scope and effective allow/deny fields in grant matching;
/// the existing access lease still records the full native revision as evidence.
pub(super) fn policy_material(policy: &Value) -> Value {
    let mut value = json!({});
    for key in ["Company","SecCode","SysRowID","EntryList","NoEntryList","SecurityMgr","AllowAll","DisallowAll"] {
        value[key] = policy[key].clone();
    }
    value
}

/// Native ObjectSecuritySvc persists permission IDs with commas. The account
/// GroupList uses a separate contract with tilde delimiters.
fn policy_ids(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|id| !id.is_empty())
}

/// Use the native service's security domain. This is not a collection roster.
fn native_security_code(service: &str) -> Option<String> {
    let parts = service.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || !matches!(parts[0], "Ice" | "Erp") {
        return None;
    }
    let domain = match parts[1] {
        "BO" => "bo",
        "Lib" => "lib",
        _ => return None,
    };
    let name = parts[2].strip_suffix("Svc")?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(format!("{domain}.{name}"))
}
fn native_policy_absent(error: &Diagnostic) -> bool {
    error.native_details.as_ref().is_some_and(|value| {
        value["ErrorType"].as_str() == Some("Ice.BLException")
            && value["ErrorMessage"].as_str() == Some("Record not found")
    })
}
fn policy_invalid() -> Diagnostic {
    Diagnostic::new(
        "ACCESS_POLICY_UNVERIFIED",
        "The Epicor policy does not establish an exact membership remedy.",
    )
}
fn controlling_policy<'a>(
    response: &'a Value,
    code: &str,
    method: &str,
    company: &str,
) -> Result<Option<&'a Value>, Diagnostic> {
    let tableset = response.get("returnObj").ok_or_else(policy_invalid)?;
    let service = tableset["Security"].as_array().ok_or_else(policy_invalid)?;
    let methods = tableset["ObjectSecurityMethod"]
        .as_array()
        .ok_or_else(policy_invalid)?;
    let method_code = format!("{code}.{method}");
    for (rows, expected) in [(methods, method_code.as_str()), (service, code)] {
        for scope in [company, ""] {
            let matches = rows
                .iter()
                .filter(|row| {
                    row["SecCode"].as_str() == Some(expected)
                        && row["Company"].as_str() == Some(scope)
                })
                .collect::<Vec<_>>();
            if matches.len() > 1 {
                return Err(policy_invalid());
            }
            if let Some(row) = matches.first() {
                if row["SysRowID"]
                    .as_str()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .is_none()
                    || !row["SysRevID"].is_number()
                    || row["SecurityMgr"].as_bool().is_none()
                {
                    return Err(policy_invalid());
                }
                return Ok(Some(row));
            }
        }
    }
    Ok(None)
}

fn authorization_refusal(error: &Diagnostic) -> bool {
    error.code == "ACCESS_DENIED"
        || error.native_details.as_ref().is_some_and(|value| {
            matches!(
                value["ErrorType"].as_str(),
                Some("Ice.Common.SecurityAccessException" | "Ice.SecurityException")
            )
        })
}

#[cfg(test)]
mod access_preview_tests {
    use super::*;
    #[tokio::test]
    async fn actual_preview_only_reads_native_policy_and_preserves_holders_sessions_and_local_files() {
        use std::{io::{BufRead,BufReader,Read,Write},net::{TcpListener,TcpStream},sync::{Mutex as StdMutex,atomic::{AtomicBool,Ordering}}};
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let address=listener.local_addr().unwrap();let endpoint=url::Url::parse(&format!("http://{address}/fixture/")).unwrap();
        let calls=Arc::new(StdMutex::new(Vec::<String>::new()));let seen=calls.clone();let stop=Arc::new(AtomicBool::new(false));let done=stop.clone();
        let worker=std::thread::spawn(move||for stream in listener.incoming() {
            let mut stream=stream.unwrap();if done.load(Ordering::SeqCst){break;}
            let mut reader=BufReader::new(&mut stream);let mut first=String::new();reader.read_line(&mut first).unwrap();let route=first.split_whitespace().nth(1).unwrap().to_owned();let method=route.rsplit('/').next().unwrap();
            let mut length=0;loop {let mut line=String::new();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}if let Some((key,value))=line.split_once(':'){if key.eq_ignore_ascii_case("content-length"){length=value.trim().parse().unwrap();}}}
            let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();drop(reader);let args:Value=serde_json::from_slice(&body).unwrap_or(Value::Null);seen.lock().unwrap().push(route.clone());
            let user=json!({"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false,"GroupList":"ELIGIBLE~UNRELATED","SysRowID":"11111111-1111-1111-1111-111111111111","SysRevID":1,"PwdChangeRequestOn":null});
            let response=match method {
                "TokenResource"=>json!({"TokenType":"Bearer","AccessToken":"fictional-token","ExpiresIn":3600}),
                "Login"=>json!({"returnObj":"fictional-session"}),
                "GetCurrentValuesFromServer"=>json!({"parameters":{"companyID":"C"}}),
                "GetDatabaseVersion"=>json!({"returnObj":"5.2.100"}),
                "GetUserFile"=>json!({"returnObj":{"UserFile":[user]}}),
                "GetByID" if route.contains("UserFileSvc")=>json!({"returnObj":{"UserFile":[user]}}),
                "GetByID" if route.contains("SecGroupSvc")=>json!({"returnObj":{"SecGroup":[{"Company":"C","SecGroupCode":args["secGroupCode"],"SysRowID":"22222222-2222-2222-2222-222222222222"}]}}),
                "GetByIDEx"=>json!({"returnObj":{"Security":[{"Company":"C","SecCode":"bo.Test","SysRowID":"33333333-3333-3333-3333-333333333333","SysRevID":1,"EntryList":"ELIGIBLE","NoEntryList":"","SecurityMgr":false,"AllowAll":false,"DisallowAll":false}],"ObjectSecurityMethod":[]}}),
                _=>json!({"unexpectedRoute":route}),
            };
            let text=serde_json::to_string(&response).unwrap();write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
        });
        // Join the loopback listener even if an assertion fails.
        struct Stop{stop:Arc<AtomicBool>,address:std::net::SocketAddr,worker:Option<std::thread::JoinHandle<()>>}
        impl Drop for Stop{fn drop(&mut self){self.stop.store(true,Ordering::SeqCst);let _=TcpStream::connect(self.address);self.worker.take().unwrap().join().unwrap();}}
        let _fixture=Stop{stop,address,worker:Some(worker)};
        let root=tempfile::tempdir().unwrap();let host=Host::new(root.path().join("state"),root.path().join("plugins"));
        let connection=Connection::new(endpoint.clone(),"C".into(),AuthMode::Session,crate::input::Credentials{username:"manager".into(),password:"fictional".into(),api_key:None},OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
        connection.connect(&CancellationToken::new()).await.unwrap();
        {
            let mut owned=host.access_store.wait(endpoint.as_str(),"manager").unwrap();
            for group in ["ELIGIBLE","UNRELATED"] {
                let lease_id=uuid::Uuid::new_v4().to_string();let artifact=Identity{category:"NativeOperation".into(),kind:"NativeMethod".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::new()};
                owned.registry.leases.insert(lease_id.clone(),crate::access::AccessLease{schema_version:1,lease_id:lease_id.clone(),endpoint:endpoint.to_string(),company:"C".into(),caller:"manager".into(),holder:"original-holder".into(),artifact,group:group.into(),native_policy:"{}".into(),state:"granted".into(),existed_before:false,membership_added:true,diagnostics:vec![],authorization:None});
                owned.registry.memberships.insert(group.into(),crate::access::Membership{group:group.into(),holders:std::collections::BTreeSet::from(["original-holder".into()]),phase:crate::access::MembershipPhase::Active,existed_before:false,membership_added:true,before_revision:"0".into(),grant_revision:Some("1".into()),intent_id:format!("intent-{group}"),lease_ids:std::collections::BTreeSet::from([lease_id]),native_write_confirmed:true,account_row_id:Some("11111111-1111-1111-1111-111111111111".into()),account_safeguards_sha256:None});
            }
            owned.checkpoint().unwrap();
        }
        fn files(root:&std::path::Path)->BTreeMap<std::path::PathBuf,Vec<u8>> {let mut out=BTreeMap::new();if root.exists(){for entry in std::fs::read_dir(root).unwrap(){let p=entry.unwrap().path();if p.is_dir(){out.extend(files(&p));}else{out.insert(p.clone(),std::fs::read(&p).unwrap());}}}out}
        let before=files(root.path());let session=host.authorization_session.read().await.clone();calls.lock().unwrap().clear();
        let methods=BTreeMap::from([("Ice.BO.TestSvc".into(),std::collections::BTreeSet::from(["GetRows".into()]))]);
        let effects=host.preview_policy_effects(&connection,&methods,&CancellationToken::new()).await.unwrap();assert_eq!(effects.len(),1);assert_eq!(effects.values().next().unwrap()["kind"],"borrowOwnedMembership");assert_eq!(effects.values().next().unwrap()["group"],"ELIGIBLE");
        assert_eq!(before,files(root.path()));assert_eq!(session,*host.authorization_session.read().await);
        let routes=calls.lock().unwrap().clone();assert_eq!(routes.len(),4);assert!(routes.iter().all(|r|r.ends_with("/GetByID")||r.ends_with("/GetByIDEx"))); // no login, refresh, borrow, write, sweep or export
    }
    #[test]
    fn owned_membership_is_a_selected_effect_permanent_membership_is_not_and_unrelated_groups_are_excluded() {
        let policy=json!({"Company":"C","SecCode":"bo.Test","SysRowID":"row","EntryList":"ELIGIBLE","NoEntryList":"","SecurityMgr":false,"AllowAll":false,"DisallowAll":false});
        let policies=||vec![("Ice.BO.TestSvc".into(),"GetRows".into(),policy.clone())];
        let canonical=BTreeMap::from([("ELIGIBLE".into(),"ELIGIBLE".into()),("UNRELATED".into(),"UNRELATED".into())]);
        let current=std::collections::BTreeSet::from(["ELIGIBLE".into(),"UNRELATED".into()]);
        let owned=BTreeMap::from([("ELIGIBLE".into(),"original-intent".into()),("UNRELATED".into(),"unrelated-intent".into())]);
        let effects=policy_access_effects(policies(),&canonical,&current,&owned).unwrap();assert_eq!(effects.len(),1);
        let effect=&effects["access:Ice.BO.TestSvc/GetRows/ELIGIBLE"];assert_eq!(effect["kind"],"borrowOwnedMembership");assert_eq!(effect["ownershipIntent"],"original-intent");
        assert!(policy_access_effects(policies(),&canonical,&current,&BTreeMap::new()).unwrap().is_empty());
        let effects=policy_access_effects(policies(),&canonical,&std::collections::BTreeSet::new(),&BTreeMap::new()).unwrap();assert_eq!(effects.values().next().unwrap()["kind"],"groupMembership");
    }
}

/// Loopback cases for exact acquisition/borrowing at the actual dispatch path.
#[cfg(test)]
mod exact_access_tests {
    use super::*;
    use std::{io::{BufRead,BufReader,Read,Write},net::{TcpListener,TcpStream},sync::{Mutex as StdMutex,atomic::{AtomicBool,Ordering}}};
    use crate::server_changes::{self,Capability,Lifetime,MaterialScope,RunAuthority};
    const ROW:&str="33333333-3333-3333-3333-333333333333";
    const ACCOUNT:&str="11111111-1111-1111-1111-111111111111";
    const BORROW_KEY:&str="access:Ice.BO.TestSvc/GetRows/ELIGIBLE";
    struct Server{groups:String,entry:String,policy_row:String,deny_reads:bool,malformed_policy:bool,calls:Vec<String>,on_read:Option<Box<dyn FnMut()+Send>>}
    struct Fixture{endpoint:url::Url,state:Arc<StdMutex<Server>>,stop:Arc<AtomicBool>,worker:Option<std::thread::JoinHandle<()>>}
    impl Fixture {
        fn new(groups:&str,entry:&str)->Self {
            let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let address=listener.local_addr().unwrap();
            let state=Arc::new(StdMutex::new(Server{groups:groups.into(),entry:entry.into(),policy_row:ROW.into(),deny_reads:false,malformed_policy:false,calls:vec![],on_read:None}));let shared=state.clone();
            let stop=Arc::new(AtomicBool::new(false));let done=stop.clone();
            let worker=std::thread::spawn(move||for stream in listener.incoming() {
                let mut stream=stream.unwrap();if done.load(Ordering::SeqCst){break;}
                let mut reader=BufReader::new(&mut stream);let mut first=String::new();reader.read_line(&mut first).unwrap();let route=first.split_whitespace().nth(1).unwrap().to_owned();let method=route.rsplit('/').next().unwrap().to_owned();
                let mut length=0;loop {let mut line=String::new();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}if let Some((key,value))=line.split_once(':'){if key.eq_ignore_ascii_case("content-length"){length=value.trim().parse().unwrap();}}}
                let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();drop(reader);let args:Value=serde_json::from_slice(&body).unwrap_or(Value::Null);
                let mut s=shared.lock().unwrap();s.calls.push(method.clone());let mut status=200;
                let user=json!({"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false,"GroupList":s.groups,"SysRowID":ACCOUNT,"SysRevID":1,"PwdChangeRequestOn":null});
                let response=match method.as_str() {
                    "GetDatabaseVersion"=>json!({"returnObj":"5.2.100"}),
                    "GetUserFile"=>json!({"returnObj":{"UserFile":[user]}}),
                    "GetByID" if route.contains("UserFileSvc")=>json!({"returnObj":{"UserFile":[user]}}),
                    "GetByID" if route.contains("SecGroupSvc")=>json!({"returnObj":{"SecGroup":[{"Company":"C","SecGroupCode":args["secGroupCode"],"SysRowID":"22222222-2222-2222-2222-222222222222"}]}}),
                    "GetByIDEx" if s.malformed_policy=>json!({"unexpected":true}),
                    "GetByIDEx"=>json!({"returnObj":{"Security":[{"Company":"C","SecCode":"bo.Test","SysRowID":s.policy_row,"SysRevID":1,"EntryList":s.entry,"NoEntryList":"","SecurityMgr":false,"AllowAll":false,"DisallowAll":false}],"ObjectSecurityMethod":[]}}),
                    "AssignGroupToUsers"=>{let group=args["groupId"].as_str().unwrap().to_owned();if !s.groups.split('~').any(|g|g==group){s.groups=if s.groups.is_empty(){group}else{format!("{}~{group}",s.groups)};}json!({})},
                    "GetRows"|"GetList" if s.deny_reads=>{status=403;json!({"ErrorMessage":"Access denied"})},
                    // The page is served, then the hook runs before the response is sent.
                    "GetRows"|"GetList"=>{if let Some(hook)=s.on_read.as_mut(){hook();}json!({"returnObj":{"Rows":[]}})},
                    _=>json!({}),
                };
                drop(s);let text=serde_json::to_string(&response).unwrap();write!(stream,"HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
            });
            Self{endpoint:url::Url::parse(&format!("http://{address}/fixture/")).unwrap(),state,stop,worker:Some(worker)}
        }
        fn calls(&self,method:&str)->usize {self.state.lock().unwrap().calls.iter().filter(|c|c.as_str()==method).count()}
        fn writes(&self)->usize {self.calls("AssignGroupToUsers")+self.calls("UpdateExt")}
    }
    impl Drop for Fixture {fn drop(&mut self){self.stop.store(true,Ordering::SeqCst);let _=TcpStream::connect(self.endpoint.socket_addrs(||None).unwrap()[0]);self.worker.take().unwrap().join().unwrap();}}

    struct Case{_root:tempfile::TempDir,host:Arc<Host>,connection:Arc<Connection>,run:Arc<Run>,run_id:String}
    async fn case(server:&Fixture)->Case {
        let root=tempfile::tempdir().unwrap();let host=Host::new(root.path().join("state"),root.path().join("plugins"));
        let connection=Connection::new(server.endpoint.clone(),"C".into(),AuthMode::BasicApiKey,crate::input::Credentials{username:"manager".into(),password:"fictional".into(),api_key:Some("fixture".into())},OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
        connection.connect(&CancellationToken::new()).await.unwrap();
        let mut journal=storage::new_journal("exact-request".into());journal.endpoint=connection.endpoint.to_string();journal.company="C".into();journal.caller="manager".into();journal.temporary_access_enabled=true;
        let run_id=journal.run_id.clone();let store=Storage::open(root.path().join("output").as_path(),false,journal).unwrap();
        let run=Arc::new(Run{storage:Arc::new(std::sync::Mutex::new(store)),cancel:CancellationToken::new(),paused:AtomicBool::new(false),wake:Notify::new(),active:AtomicBool::new(true),network:true,helper_restore_attempts:Mutex::new(Default::default()),activity:Arc::new(Default::default()),shared_reads:Default::default(),helper_instances:Mutex::new(BTreeMap::new()),collection_context:Mutex::new(None)});
        Case{_root:root,host,connection,run,run_id}
    }
    /// Another run's confirmed owned addition, as the existing registry records it.
    fn seed_owned(host:&Host,endpoint:&url::Url,group:&str,holder:&str,intent:&str) {
        let mut owned=host.access_store.wait(endpoint.as_str(),"manager").unwrap();let lease_id=uuid::Uuid::new_v4().to_string();
        let artifact=Identity{category:"NativeOperation".into(),kind:"NativeMethod".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::new()};
        owned.registry.leases.insert(lease_id.clone(),crate::access::AccessLease{schema_version:1,lease_id:lease_id.clone(),endpoint:endpoint.to_string(),company:"C".into(),caller:"manager".into(),holder:holder.into(),artifact,group:group.into(),native_policy:"{}".into(),state:"granted".into(),existed_before:false,membership_added:true,diagnostics:vec![],authorization:None});
        owned.registry.memberships.insert(group.into(),crate::access::Membership{group:group.into(),holders:std::collections::BTreeSet::from([holder.into()]),phase:crate::access::MembershipPhase::Active,existed_before:false,membership_added:true,before_revision:"0".into(),grant_revision:Some("1".into()),intent_id:intent.into(),lease_ids:std::collections::BTreeSet::from([lease_id]),native_write_confirmed:true,account_row_id:Some(ACCOUNT.into()),account_safeguards_sha256:None});
        owned.checkpoint().unwrap();
    }
    fn release_owned(host:&Host,endpoint:&url::Url,group:&str) {
        let mut owned=host.access_store.wait(endpoint.as_str(),"manager").unwrap();let member=owned.registry.memberships.get_mut(group).unwrap();member.phase=crate::access::MembershipPhase::Released;member.holders.clear();owned.checkpoint().unwrap();
    }
    fn registry(host:&Host,endpoint:&url::Url)->crate::access::AccessRegistry {host.access_store.snapshot(endpoint.as_str(),"manager").unwrap()}
    /// A one-run grant over exactly the supplied effects, attached to the run.
    async fn authorize(case:&Case,effects:BTreeMap<String,Value>)->(String,MaterialScope) {
        let scope=MaterialScope{endpoint:case.connection.endpoint.to_string(),company:"C".into(),principal:"manager".into(),capabilities:BTreeMap::from([("$native-access".into(),Capability{binding:"stored-native-policy-remedy-v1".into(),selection:BTreeMap::new(),effects})])};
        let grant=server_changes::create(&case.host.state_root,scope.clone(),Lifetime::OneRun{request_id:"exact-request".into()},None,"explicitLocalClient").unwrap();
        let session=case.host.authorization_session.read().await.clone();
        {let mut storage=case.run.storage.lock().unwrap();storage.journal.server_change_authority=Some(RunAuthority{grant_id:grant.id.clone(),material_digest:scope.digest(),material_scope:scope.clone(),request_id:"exact-request".into(),original_session:session,workflow_id:None});storage.checkpoint().unwrap();}
        (grant.id,scope)
    }
    async fn previewed(case:&Case)->BTreeMap<String,Value> {
        let methods=BTreeMap::from([("Ice.BO.TestSvc".into(),std::collections::BTreeSet::from(["GetRows".into()]))]);
        case.host.preview_policy_effects(&case.connection,&methods,&CancellationToken::new()).await.unwrap()
    }
    async fn read(case:&Case)->Result<(),Diagnostic> {read_method(case,"GetRows").await}
    async fn read_method(case:&Case,method:&str)->Result<(),Diagnostic> {
        case.host.native_read_with_access(&case.connection,"Ice.BO.TestSvc",method,json!({}),false,Some(&case.run),None,&case.run_id,true,&CancellationToken::new()).await.map(|_|())
    }
    fn set_phase(host:&Host,endpoint:&url::Url,group:&str,phase:crate::access::MembershipPhase) {
        let mut owned=host.access_store.wait(endpoint.as_str(),"manager").unwrap();owned.registry.memberships.get_mut(group).unwrap().phase=phase;owned.checkpoint().unwrap();
    }
    fn set_intent(host:&Host,endpoint:&url::Url,group:&str,intent:&str) {
        let mut owned=host.access_store.wait(endpoint.as_str(),"manager").unwrap();owned.registry.memberships.get_mut(group).unwrap().intent_id=intent.into();owned.checkpoint().unwrap();
    }
    fn unheld_diagnostics(case:&Case)->Vec<Diagnostic> {case.run.storage.lock().unwrap().journal.diagnostics.iter().filter(|d|d.code==UNHELD_MEMBERSHIP).cloned().collect()}
    fn memo(case:&Case)->std::collections::BTreeSet<String> {case.host.borrow_preview_negative.lock().unwrap().get(&case.run_id).cloned().unwrap_or_default()}

    #[tokio::test]
    async fn unauthorized_borrow_reads_without_holder_and_records_one_reliance_diagnostic() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        let before=registry(&case.host,&case.connection.endpoint);
        for _ in 0..2 {read(&case).await.unwrap();}
        let after=registry(&case.host,&case.connection.endpoint);
        assert_eq!(serde_json::to_value(&before).unwrap(),serde_json::to_value(&after).unwrap(),"no holder, lease or membership change");
        let diagnostics=unheld_diagnostics(&case);assert_eq!(diagnostics.len(),1,"one per run, group and ownership generation");
        assert_eq!(diagnostics[0].context["group"],"ELIGIBLE");assert_eq!(diagnostics[0].context["ownershipIntent"],"other-intent");
        assert_eq!(server.writes(),0);assert_eq!(server.calls("GetRows"),2);
        assert!(memo(&case).is_empty(),"an outcome that found a borrow effect is not memoised");
    }

    #[tokio::test]
    async fn authorized_borrow_holds_with_reference_but_revoked_or_drifted_authority_refuses_before_read() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        let effects=previewed(&case).await;assert_eq!(effects[BORROW_KEY]["kind"],"borrowOwnedMembership");
        let (grant_id,scope)=authorize(&case,effects.clone()).await;
        read(&case).await.unwrap();
        let held=registry(&case.host,&case.connection.endpoint);assert!(held.memberships["ELIGIBLE"].holders.contains(&case.run_id));
        let lease=held.leases.values().find(|l|l.holder==case.run_id).unwrap();
        assert_eq!(lease.authorization,Some(crate::server_changes::AuthorizationRef{grant_id:grant_id.clone(),material_digest:scope.digest(),request_id:"exact-request".into()}));
        assert_eq!(held.memberships["ELIGIBLE"].intent_id,"other-intent","borrowing keeps the original ownership generation");
        assert_eq!(server.writes(),0);assert!(unheld_diagnostics(&case).is_empty());

        // Revoked: same unheld shape on a fresh run, refuse before the read.
        let revoked=self::case(&server).await;seed_owned(&revoked.host,&revoked.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        let (id,_)=authorize(&revoked,effects.clone()).await;server_changes::revoke(&revoked.host.state_root,&id).unwrap();
        let reads=server.calls("GetRows");let error=read(&revoked).await.unwrap_err();
        assert_eq!(error.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED");assert_eq!(server.calls("GetRows"),reads,"refused before the native read");
        assert!(unheld_diagnostics(&revoked).is_empty());assert!(!registry(&revoked.host,&revoked.connection.endpoint).memberships["ELIGIBLE"].holders.contains(&revoked.run_id));

        // Drifted controlling policy facts: refuse before the read.
        let drifted=self::case(&server).await;seed_owned(&drifted.host,&drifted.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        authorize(&drifted,effects).await;server.state.lock().unwrap().entry="ELIGIBLE,LATER".into();
        let reads=server.calls("GetRows");let error=read(&drifted).await.unwrap_err();
        assert_eq!(error.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED");assert_eq!(server.calls("GetRows"),reads);
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn owner_release_mid_pagination_fails_the_page_with_attributed_reliance_and_no_grant() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        read(&case).await.unwrap(); // page 1 through the unheld membership
        release_owned(&case.host,&case.connection.endpoint,"ELIGIBLE");
        {let mut s=server.state.lock().unwrap();s.groups=String::new();s.deny_reads=true;}
        let error=read(&case).await.unwrap_err(); // page 2 after the owner released it
        let relied=error.context.get("unheldMemberships").and_then(Value::as_array).cloned().unwrap_or_default();
        assert_eq!(relied,vec![json!({"group":"ELIGIBLE","ownershipIntent":"other-intent"})],"{error:?}");
        assert_eq!(server.calls("AssignGroupToUsers"),0,"no authority to add the released group");assert_eq!(server.writes(),0);
        assert!(!registry(&case.host,&case.connection.endpoint).memberships["ELIGIBLE"].holders.contains(&case.run_id));
    }

    #[tokio::test]
    async fn negative_borrow_preview_is_reused_only_for_the_same_unheld_set_and_evicted_on_refusal() {
        // OTHER is unrelated to this method's policy, so no borrow effect exists.
        let server=Fixture::new("OTHER","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"OTHER","other-run","intent-1");
        read(&case).await.unwrap();read(&case).await.unwrap();
        assert_eq!(server.calls("GetByIDEx"),1,"unchanged unheld set reuses the negative preview");assert_eq!(memo(&case).len(),1);
        // New ownership generation for the same group.
        release_owned(&case.host,&case.connection.endpoint,"OTHER");seed_owned(&case.host,&case.connection.endpoint,"OTHER","other-run","intent-2");
        read(&case).await.unwrap();assert_eq!(server.calls("GetByIDEx"),2,"changed intent_id re-previews");
        // A newly unheld group.
        server.state.lock().unwrap().groups="OTHER~SECOND".into();seed_owned(&case.host,&case.connection.endpoint,"SECOND","third-run","intent-3");
        read(&case).await.unwrap();assert_eq!(server.calls("GetByIDEx"),3,"newly unheld group re-previews");
        // An authorization refusal evicts this method's entries.
        server.state.lock().unwrap().deny_reads=true;assert!(read(&case).await.is_err());
        assert!(memo(&case).iter().all(|key|!key.starts_with("Ice.BO.TestSvc\u{1f}GetRows\u{1f}")),"refusal evicts");
        server.state.lock().unwrap().deny_reads=false;let previews=server.calls("GetByIDEx");read(&case).await.unwrap();assert_eq!(server.calls("GetByIDEx"),previews+1,"evicted entry is previewed again");
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn borrow_preview_errors_are_not_memoised() {
        let server=Fixture::new("OTHER","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"OTHER","other-run","intent-1");
        server.state.lock().unwrap().malformed_policy=true;
        assert_eq!(read(&case).await.unwrap_err().code,"ACCESS_POLICY_UNVERIFIED");assert!(memo(&case).is_empty());
        server.state.lock().unwrap().malformed_policy=false;read(&case).await.unwrap();assert_eq!(memo(&case).len(),1);
    }

    fn requirement(group:&str)->crate::native_access::Requirement {
        crate::native_access::Requirement{group:group.into(),native_policy:"{}".into(),artifact:Identity{category:"NativeOperation".into(),kind:"NativeMethod".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::new()}}
    }
    /// The groupMembership effect exactly as preview derives it for an absent group.
    async fn add_effect(case:&Case)->(String,Value) {let effects=previewed(case).await;let (key,effect)=effects.into_iter().next().unwrap();assert_eq!(effect["kind"],"groupMembership");(key,effect)}

    #[tokio::test]
    async fn grant_refuses_joining_another_runs_owned_membership_without_borrow_effect() {
        let server=Fixture::new("","ELIGIBLE");let case=case(&server).await;
        let (key,effect)=add_effect(&case).await;let (grant_id,scope)=authorize(&case,BTreeMap::from([(key.clone(),effect)])).await;
        // Another run's addition became active while this request was queued.
        seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");server.state.lock().unwrap().groups="ELIGIBLE".into();
        let before=registry(&case.host,&case.connection.endpoint);let session=case.host.authorization_session.read().await.clone();
        let authority=crate::native_access::PolicyAuthority{grants_root:&case.host.state_root,grant_id:&grant_id,scope:&scope,use_:crate::server_changes::Use{request_id:"exact-request",session_id:&session,workflow_id:None},temporary_access:true,selected_effects:std::collections::BTreeSet::from([key])};
        let error=crate::native_access::grant(&case.host.access_store,&case.connection,&case.run_id,requirement("ELIGIBLE"),&authority,&CancellationToken::new()).await.unwrap_err();
        assert!(error.message.contains("not joining another run"),"{error:?}");
        let after=registry(&case.host,&case.connection.endpoint);
        assert_eq!(serde_json::to_value(&before).unwrap(),serde_json::to_value(&after).unwrap(),"holders and leases unchanged");
        assert_eq!(after.memberships["ELIGIBLE"].holders,std::collections::BTreeSet::from(["other-run".to_string()]));assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn grant_rechecks_controlling_policy_under_the_account_lock_before_writing() {
        let server=Fixture::new("","ELIGIBLE");let case=case(&server).await;
        let (key,effect)=add_effect(&case).await;let (grant_id,scope)=authorize(&case,BTreeMap::from([(key.clone(),effect)])).await;
        let session=case.host.authorization_session.read().await.clone();
        let authority=crate::native_access::PolicyAuthority{grants_root:&case.host.state_root,grant_id:&grant_id,scope:&scope,use_:crate::server_changes::Use{request_id:"exact-request",session_id:&session,workflow_id:None},temporary_access:true,selected_effects:std::collections::BTreeSet::from([key])};
        // The controlling policy row changed after preview/authorization.
        server.state.lock().unwrap().policy_row="44444444-4444-4444-4444-444444444444".into();
        let error=crate::native_access::grant(&case.host.access_store,&case.connection,&case.run_id,requirement("ELIGIBLE"),&authority,&CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED","{error:?}");assert_eq!(server.writes(),0);
        assert!(!registry(&case.host,&case.connection.endpoint).memberships.contains_key("ELIGIBLE"),"no grant intent left behind");
        // Unchanged policy: one exact write, lease attributed to the run grant.
        server.state.lock().unwrap().policy_row=ROW.into();
        let lease=crate::native_access::grant(&case.host.access_store,&case.connection,&case.run_id,requirement("ELIGIBLE"),&authority,&CancellationToken::new()).await.unwrap();
        assert_eq!(server.calls("AssignGroupToUsers"),1);
        assert_eq!(lease.authorization,Some(crate::server_changes::AuthorizationRef{grant_id,material_digest:scope.digest(),request_id:"exact-request".into()}));
        assert_eq!(registry(&case.host,&case.connection.endpoint).memberships["ELIGIBLE"].phase,crate::access::MembershipPhase::Active);
    }

    #[tokio::test]
    async fn grant_refuses_other_target_company_or_caller_before_the_account_lock() {
        let server=Fixture::new("","ELIGIBLE");let case=case(&server).await;
        let (key,effect)=add_effect(&case).await;let (grant_id,mut scope)=authorize(&case,BTreeMap::from([(key.clone(),effect)])).await;
        scope.principal="someone-else".into();
        let grant=server_changes::create(&case.host.state_root,scope.clone(),Lifetime::OneRun{request_id:"exact-request".into()},None,"explicitLocalClient").unwrap();let _=grant_id;
        let session=case.host.authorization_session.read().await.clone();
        let authority=crate::native_access::PolicyAuthority{grants_root:&case.host.state_root,grant_id:&grant.id,scope:&scope,use_:crate::server_changes::Use{request_id:"exact-request",session_id:&session,workflow_id:None},temporary_access:true,selected_effects:std::collections::BTreeSet::from([key])};
        let users=server.calls("GetByID");
        assert!(crate::native_access::grant(&case.host.access_store,&case.connection,&case.run_id,requirement("ELIGIBLE"),&authority,&CancellationToken::new()).await.is_err());
        assert_eq!(server.calls("GetByID"),users,"no account read under the lock");assert_eq!(server.writes(),0);
        assert!(registry(&case.host,&case.connection.endpoint).memberships.is_empty());
    }

    #[tokio::test]
    async fn authorize_native_policy_requires_an_authorized_run_and_its_own_holder() {
        let server=Fixture::new("","ELIGIBLE");let case=case(&server).await;
        let error=case.host.authorize_native_policy(&case.connection,"Ice.BO.TestSvc","GetRows",&case.run_id,None,None,&CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED");
        let (key,effect)=add_effect(&case).await;authorize(&case,BTreeMap::from([(key,effect)])).await;
        let error=case.host.authorize_native_policy(&case.connection,"Ice.BO.TestSvc","GetRows","another-holder",Some(&case.run),None,&CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.code,"ACCESS_SCOPE_MISMATCH");
        let fresh=self::case(&server).await;let unauthorized=fresh.host.authorize_native_policy(&fresh.connection,"Ice.BO.TestSvc","GetRows",&fresh.run_id,Some(&fresh.run),None,&CancellationToken::new()).await.unwrap_err();
        assert_eq!(unauthorized.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED","a run without authority cannot add membership");
        assert_eq!(server.writes(),0);
    }

    // Exact-access review carry-ins (a)-(d).

    #[tokio::test]
    async fn memo_never_skips_ownership_uncertainty_check() {
        let server=Fixture::new("OTHER","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"OTHER","other-run","intent-1");
        read(&case).await.unwrap();assert_eq!(memo(&case).len(),1,"negative preview memoised");
        // Another run's addition of a group this account currently has becomes uncertain.
        seed_owned(&case.host,&case.connection.endpoint,"THIRD","third-run","intent-3");set_phase(&case.host,&case.connection.endpoint,"THIRD",crate::access::MembershipPhase::Uncertain);
        server.state.lock().unwrap().groups="OTHER~THIRD".into();
        let previews=server.calls("GetByIDEx");let reads=server.calls("GetRows");
        assert_eq!(read(&case).await.unwrap_err().code,"ACCESS_OWNERSHIP_UNCERTAIN");
        assert_eq!(server.calls("GetByIDEx"),previews+1,"the memo was not consulted");assert_eq!(server.calls("GetRows"),reads,"refused before the read");
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn memo_key_changes_when_any_owned_group_phase_changes() {
        let server=Fixture::new("OTHER~MINE","ELIGIBLE");let case=case(&server).await;
        seed_owned(&case.host,&case.connection.endpoint,"OTHER","other-run","intent-1");seed_owned(&case.host,&case.connection.endpoint,"MINE",&case.run_id,"mine-1");
        read(&case).await.unwrap();read(&case).await.unwrap();assert_eq!(server.calls("GetByIDEx"),1,"unchanged owned registry reuses the memo");
        // A group this run already holds is not unheld, but the key still covers it.
        set_intent(&case.host,&case.connection.endpoint,"MINE","mine-2");
        read(&case).await.unwrap();assert_eq!(server.calls("GetByIDEx"),2,"new ownership generation re-previews");
        // Any owned group that is not Active disables the memo (no read, no write).
        server.state.lock().unwrap().groups="OTHER".into();set_phase(&case.host,&case.connection.endpoint,"MINE",crate::access::MembershipPhase::CleanupPending);
        let entries=memo(&case).len();
        read(&case).await.unwrap();read(&case).await.unwrap();
        assert_eq!(server.calls("GetByIDEx"),4,"every call previews while a phase is unresolved");assert_eq!(memo(&case).len(),entries,"nothing new is memoised");
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn memo_entries_are_dropped_when_the_run_finishes_cancels_or_fails() {
        let server=Fixture::new("","ELIGIBLE");
        for ending in ["finished","canceled","failed"] {
            let case=case(&server).await;
            case.host.borrow_preview_negative.lock().unwrap().entry(case.run_id.clone()).or_default().insert("Ice.BO.TestSvc\u{1f}GetRows\u{1f}{}".into());
            if ending=="canceled" {case.run.cancel.cancel();}
            let categories=if ending=="failed" {vec!["not-installed".to_string()]} else {vec![]};
            case.host.clone().collect(case.run.clone(),case.connection.clone(),categories,vec![],false,None).await;
            assert!(memo(&case).is_empty(),"{ending}: the run's memo is dropped at its terminal path");
            assert!(!case.run.active.load(Ordering::SeqCst),"{ending}");
        }
        // Recovery start drops it too.
        let case=case(&server).await;
        case.host.borrow_preview_negative.lock().unwrap().entry(case.run_id.clone()).or_default().insert("Ice.BO.TestSvc\u{1f}GetRows\u{1f}{}".into());
        let _=case.host.recover(&case.run_id,"run.recover").await;
        assert!(memo(&case).is_empty(),"recovery start re-evaluates every preview");
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn attribution_lists_only_the_refused_methods_reliance() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        read_method(&case,"GetRows").await.unwrap(); // only GetRows relied on the membership
        let diagnostics=unheld_diagnostics(&case);assert_eq!(diagnostics.len(),1);
        assert_eq!(diagnostics[0].context["service"],"Ice.BO.TestSvc");assert_eq!(diagnostics[0].context["method"],"GetRows");
        release_owned(&case.host,&case.connection.endpoint,"ELIGIBLE");
        {let mut s=server.state.lock().unwrap();s.groups=String::new();s.deny_reads=true;}
        let other=read_method(&case,"GetList").await.unwrap_err();
        assert!(other.context.get("unheldMemberships").is_none(),"GetList never relied on it: {other:?}");
        let relied=read_method(&case,"GetRows").await.unwrap_err();
        assert_eq!(relied.context["unheldMemberships"],json!([{"group":"ELIGIBLE","ownershipIntent":"other-intent"}]),"{relied:?}");
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn release_during_unheld_read_marks_the_page_incomplete_even_when_rows_return() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        let (host,endpoint)=(case.host.clone(),case.connection.endpoint.clone());
        server.state.lock().unwrap().on_read=Some(Box::new(move||release_owned(&host,&endpoint,"ELIGIBLE")));
        let error=read(&case).await.unwrap_err();
        assert_eq!(error.code,"ACCESS_MEMBERSHIP_LOST_DURING_READ","{error:?}");assert!(error.retryable);
        assert_eq!(error.context["unheldMemberships"],json!([{"group":"ELIGIBLE","ownershipIntent":"other-intent"}]));
        assert_eq!(error.context["service"],"Ice.BO.TestSvc");assert_eq!(error.context["method"],"GetRows");
        assert_eq!(server.calls("GetRows"),1,"rows were returned and the page is still not complete");
        assert_eq!(server.writes(),0);assert!(!registry(&case.host,&case.connection.endpoint).memberships["ELIGIBLE"].holders.contains(&case.run_id));
    }

    #[tokio::test]
    async fn borrow_dispatch_refuses_run_target_or_caller_mismatch_before_read() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");
        for field in ["caller","endpoint","company"] {
            let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
            let effects=previewed(&case).await;authorize(&case,effects).await;
            // In memory only: the checkpoint chain itself refuses a run whose scope changed.
            {let mut storage=case.run.storage.lock().unwrap();match field {"caller"=>storage.journal.caller="someone-else".into(),"endpoint"=>storage.journal.endpoint="http://other.invalid/fixture/".into(),_=>storage.journal.company="OTHER".into()};}
            let before=registry(&case.host,&case.connection.endpoint);let reads=server.calls("GetRows");
            let error=read(&case).await.unwrap_err();
            assert_eq!(error.code,"ACCESS_SCOPE_MISMATCH","{field}: {error:?}");assert_eq!(server.calls("GetRows"),reads,"{field}: refused before the read");
            assert_eq!(serde_json::to_value(&before).unwrap(),serde_json::to_value(registry(&case.host,&case.connection.endpoint)).unwrap(),"{field}: no holder change");
        }
        assert_eq!(server.writes(),0);
    }

    #[tokio::test]
    async fn borrow_dispatch_refuses_binding_drift_before_read() {
        let server=Fixture::new("ELIGIBLE","ELIGIBLE");let case=case(&server).await;seed_owned(&case.host,&case.connection.endpoint,"ELIGIBLE","other-run","other-intent");
        let effects=previewed(&case).await;
        // A grant whose capability binding is not the stored-policy remedy contract.
        let scope=MaterialScope{endpoint:case.connection.endpoint.to_string(),company:"C".into(),principal:"manager".into(),capabilities:BTreeMap::from([("$native-access".into(),Capability{binding:"changed-remedy-contract".into(),selection:BTreeMap::new(),effects})])};
        let grant=server_changes::create(&case.host.state_root,scope.clone(),Lifetime::OneRun{request_id:"exact-request".into()},None,"explicitLocalClient").unwrap();
        let session=case.host.authorization_session.read().await.clone();
        {let mut storage=case.run.storage.lock().unwrap();storage.journal.server_change_authority=Some(RunAuthority{grant_id:grant.id,material_digest:scope.digest(),material_scope:scope,request_id:"exact-request".into(),original_session:session,workflow_id:None});storage.checkpoint().unwrap();}
        let before=registry(&case.host,&case.connection.endpoint);let reads=server.calls("GetRows");
        let error=read(&case).await.unwrap_err();
        assert_eq!(error.code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED","{error:?}");assert_eq!(server.calls("GetRows"),reads,"refused before the read");
        assert_eq!(serde_json::to_value(&before).unwrap(),serde_json::to_value(registry(&case.host,&case.connection.endpoint)).unwrap(),"no holder change");
        assert!(unheld_diagnostics(&case).is_empty(),"drift is a denial, not an unauthorized reliance");assert_eq!(server.writes(),0);
    }
}
