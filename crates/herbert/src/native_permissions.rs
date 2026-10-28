//! Apply native-policy group remedies through the same durable ownership path.
//! A plugin's requested method is still checked before this adapter is called.
use super::*;

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
        let read = connection.native_read(service, method, arguments.clone(), export, cancel);
        let result = if let Some(run) = run {
            crate::activity::TRANSPORT_LOG.scope(
                (run.activity.clone(), service.into(), method.into()), read).await
        } else { read.await };
        let Err(refusal) = result else {
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
            let result = crate::native_access::grant(
                &self.access_store,
                connection,
                holder,
                crate::native_access::Requirement {
                    group,
                    native_policy: evidence.to_string(),
                    artifact,
                },
                cancel,
            )
            .await;
            if let Some(run) = run {
                self.checkpoint_access(run, connection).await?;
            }
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
                "Native helper removal is pending. Keep the owned access lease for cleanup.",
            ));
        }
        if journal
            .helper_transfers
            .iter()
            .any(|transfer| !matches!(transfer.phase.as_str(), "closed" | "notAdmitted"))
        {
            return Err(Diagnostic::new(
                "ACCESS_CLEANUP_WAITING_FOR_TRANSFERS",
                "Server transfer cleanup is pending. Keep the access lease so the original caller can finish it.",
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
                        "condition":if current.contains(group)&&owned.contains_key(group){"Before a selected policy-supported method uses another run’s owned temporary membership"}else{"Only after a qualifying native access refusal"}}));
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
        "The native policy does not establish an exact membership remedy.",
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
                owned.registry.leases.insert(lease_id.clone(),crate::access::AccessLease{schema_version:1,lease_id:lease_id.clone(),endpoint:endpoint.to_string(),company:"C".into(),caller:"manager".into(),holder:"original-holder".into(),artifact,group:group.into(),native_policy:"{}".into(),state:"granted".into(),existed_before:false,membership_added:true,diagnostics:vec![]});
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
