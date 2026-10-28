//! Checks at existing mutation boundaries. These checks do not own resources.
use super::*;
use crate::server_changes::{RunAuthority,Targets,Selection};
tokio::task_local! { static OWNED_CLEANUP:crate::server_changes::AuthorizationRef; }
pub(super) fn cleanup_reference()->Option<crate::server_changes::AuthorizationRef> {OWNED_CLEANUP.try_with(Clone::clone).ok()}
fn denied(message:&str)->Diagnostic{Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED",message)}
impl Host {
    pub(super) async fn require_server_change(
        &self,run:&Arc<Run>,connection:&Arc<Connection>,category:&str,key:&str,
        binding:Option<&str>,expected:&Value,identity:Option<&Identity>,
    )->Result<RunAuthority,Diagnostic> {
        let journal=collection::snapshot(run).await?;
        if !run.network {return Err(denied("Local source reprocessing cannot initiate server changes."));}
        if (key.starts_with("helper:")||key.starts_with("legacyHelper:")) && journal.collection_method!=CollectionMethod::ServerAssisted {
            return Err(denied("Direct collection cannot install or invoke Herbert helpers, even when a grant permits helpers."));
        }
        if key.starts_with("access:")&&!journal.temporary_access_enabled {return Err(denied("Temporary access is off. This run cannot add or extend group membership."));}
        let authority=journal.server_change_authority.ok_or_else(||denied("This run has no explicit server-change authorization. Preserve its output and review a defined preview."))?;
        if authority.request_id!=journal.request_id || authority.material_scope.endpoint!=journal.endpoint || authority.material_scope.company!=journal.company || authority.material_scope.principal!=journal.caller
            || connection.endpoint.as_str()!=journal.endpoint || connection.company!=journal.company || connection.caller().await.as_deref()!=Some(journal.caller.as_str()) {
            return Err(denied("The mutation does not belong to this logical run, target, company and authenticated caller."));
        }
        let grant=authority.check(&self.state_root,&self.authorization_session.read().await)?;
        let capability=authority.material_scope.capabilities.get(category).ok_or_else(||denied("The mutation category is outside this run’s authorized material scope."))?;
        if binding.is_some_and(|binding|binding!=capability.binding) {return Err(denied("The relevant module or declaration changed. Review renewed authority before using it."));}
        let effect=capability.effects.get(key).ok_or_else(||denied("This concrete mutation is outside the selected authorized effects."))?;
        if !expected.as_object().is_some_and(|fields|fields.iter().all(|(key,value)|effect.get(key)==Some(value))) {return Err(denied("The concrete native effect or package no longer matches its authorized material facts."));}
        if let Some(identity)=identity {
            let mut actual=authority.material_scope.clone();
            let selected=actual.capabilities.get_mut(category).expect("checked capability");
            selected.selection.insert("$nativeTargets".into(),Selection::NativeTargets{category:category.into(),targets:Targets::Exact{values:std::collections::BTreeSet::from([serde_json::to_string(identity).expect("native identity JSON")])}});
            if !authority.material_scope.contains(&actual) || !grant.scope.contains(&actual) {return Err(denied("The concrete artifact is outside this run’s selected native targets."));}
        }
        Ok(authority)
    }
    pub(super) async fn require_owned_cleanup(&self,run:&Arc<Run>,connection:&Arc<Connection>,reference:&crate::server_changes::AuthorizationRef)->Result<(),Diagnostic> {
        let journal=collection::snapshot(run).await?;
        let authority=journal.server_change_authority.iter().chain(journal.server_change_authority_history.iter()).find(|authority|reference.matches(authority)).ok_or_else(||denied("The original run scope for this resource is unavailable. Preserve its ownership evidence and explicitly authorize recovery."))?;
        if authority.request_id!=journal.request_id || authority.material_scope.endpoint!=journal.endpoint || authority.material_scope.company!=journal.company || authority.material_scope.principal!=journal.caller
            || connection.endpoint.as_str()!=journal.endpoint || connection.company!=journal.company || connection.caller().await.as_deref()!=Some(journal.caller.as_str()) {
            return Err(denied("Cleanup must use the original owned operation, target, company and authenticated caller."));
        }
        authority.cleanup_grant(&self.state_root)?;
        // Exact native ownership and cleanup phase are verified by the existing
        // artifact/access/transfer/export lifecycle before any removal.
        Ok(())
    }
    pub(super) async fn with_owned_cleanup<T>(&self,run:&Arc<Run>,connection:&Arc<Connection>,reference:crate::server_changes::AuthorizationRef,work:impl std::future::Future<Output=Result<T,Diagnostic>>)->Result<T,Diagnostic> {
        self.require_owned_cleanup(run,connection,&reference).await?;
        OWNED_CLEANUP.scope(reference,work).await
    }
    pub(super) async fn require_artifact_change(&self,run:&Arc<Run>,connection:&Arc<Connection>,owner:&str,declaration:&NativeArtifactDeclaration,source:&[u8],version:&str)->Result<RunAuthority,Diagnostic> {
        let journal=collection::snapshot(run).await?;
        let key=format!("helper:{}",declaration.alias);
        let expected=json!({"declaration":declaration,"packageFile":declaration.file_for_version(version),"packageSha256":storage::sha256(source)});
        if let Some(reference)=cleanup_reference() {
            self.require_owned_cleanup(run,connection,&reference).await?;
            let authority=journal.server_change_authority.iter().chain(journal.server_change_authority_history.iter()).find(|authority|reference.matches(authority)).expect("validated original scope");
            let effect=authority.material_scope.capabilities.get(owner).and_then(|capability|capability.effects.get(&key));
            if !effect.is_some_and(|effect|expected.as_object().unwrap().iter().all(|(field,value)|effect.get(field)==Some(value))) {return Err(denied("This cleanup mechanism needs a helper package outside the original authority. Preserve the owned resource and review applicable recovery authority."));}
            return Ok(authority.clone());
        }
        if owner!=host_utility::OWNER {
            let authority=journal.server_change_authority.as_ref().ok_or_else(||denied("Helper work requires this run’s explicit authorization."))?;
            let module=authority.material_scope.capabilities.get(owner).and_then(|capability|capability.selection.get("$moduleSha256"));
            if !journal.plugin_hashes.get(owner).is_some_and(|hash|matches!(module,Some(Selection::Exact{value}) if value.as_str()==Some(hash.as_str()))) {return Err(denied("The helper’s actual frozen module does not match the authorized module."));}
        }
        self.require_server_change(run,connection,owner,&key,None,&expected,None).await
    }

}
