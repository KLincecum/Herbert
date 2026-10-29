//! Narrow server-change authority. Resource ownership remains in existing journals,
//! native artifact operations, transfers and access/server leases.
use crate::{plugin_state::StateFile, storage};
use herbert_contract::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::{BTreeMap, BTreeSet}, path::Path, time::{SystemTime, UNIX_EPOCH}};

fn refusal(message: &str) -> Diagnostic {
    Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED", message)
}
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() }

/// The host planner assigns semantics only where the owning implementation has
/// established them. Unknown plugin controls retain exact-value semantics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag="kind", rename_all="camelCase", deny_unknown_fields)]
pub(crate) enum Selection {
    Exact { value: Value },
    LiteralPrefix { value: String, contract: String },
    Included { value: bool },
    Members { values: BTreeSet<String> },
    NativeTargets { category: String, targets: Targets },
}
impl Selection {
    fn contains(&self, requested: &Self) -> bool {
        match (self, requested) {
            (Self::Exact { value: a }, Self::Exact { value: b }) => a == b,
            (Self::LiteralPrefix { value: a, contract: ac }, Self::LiteralPrefix { value: b, contract: bc }) => ac == bc && b.starts_with(a),
            (Self::Included { value: a }, Self::Included { value: b }) => *a || !*b,
            (Self::Members { values: a }, Self::Members { values: b }) => b.is_subset(a),
            (Self::NativeTargets{category:a,targets:at},Self::NativeTargets{category:b,targets:bt})=>a==b && at.contains(a,bt),
            _ => false,
        }
    }
}

/// Discovery controls are discarded for exact acquisition by the owning native
/// collectors. Processing/content controls remain in Capability.selection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag="kind",rename_all="camelCase",deny_unknown_fields)]
pub(crate) enum Targets {
    Discovery { selection: BTreeMap<String,Selection> },
    Exact { values: BTreeSet<String> },
}
impl Targets {
    fn contains(&self,category:&str,requested:&Self) -> bool {
        match (self,requested) {
            (Self::Exact{values:a},Self::Exact{values:b})=>b.is_subset(a),
            (Self::Discovery{selection:a},Self::Discovery{selection:b})=>a.len()==b.len() && b.iter().all(|(k,v)|a.get(k).is_some_and(|g|g.contains(v))),
            (Self::Discovery{selection},Self::Exact{values})=>values.iter().all(|text|serde_json::from_str::<herbert_contract::Identity>(text).ok().is_some_and(|id|id.category==category && discovery_contains_identity(category,selection,&id))),
            _=>false,
        }
    }
}
fn discovery_contains_identity(category:&str,selection:&BTreeMap<String,Selection>,id:&herbert_contract::Identity) -> bool {
    // Reviewed native root identity mappings. Unknown roots remain equality-only;
    // a similarly named key is not evidence that its values are discovery IDs.
    // Composite identity keys and scope remain intact in Exact target sets.
    let (family, prefixes):(&str,&[(&str,&str)])=match (category,id.kind.as_str()) {
        ("ud-codes","UDCodeType")=>("",&[("codeTypeIdPrefix","CodeTypeID")]),
        ("ud-schema","UDTableSchema")=>("schema",&[("schemaTableIdPrefix","DataTableID")]),
        ("ud-schema","UDMapping")=>("mappings",&[]),
        ("extended-properties","ExtendedTableProperties")=>("",&[("tableIdPrefix","DataTableID")]),
        ("baqs-and-ubaqs","BAQ"|"UBAQ")=>("",&[("queryIdPrefix","QueryID")]),
        ("bpm-directives","MethodDirectiveGroup")=>("method",&[("methodCodePrefix","BpMethodCode")]),
        ("bpm-directives","DataDirectiveGroup")=>("data",&[("methodCodePrefix","BpMethodCode")]),
        ("functions","EfxLibrary")=>("",&[("libraryIdPrefix","LibraryID")]),
        ("classic-customizations","PersistedXXXDef")=>("",&[("layerNamePrefix","Key1"),("formNamePrefix","Key2")]),
        ("classic-personalizations","PersistedXXXDef")=>("",&[("userIdPrefix","Key1"),("formNamePrefix","Key2")]),
        ("classic-dashboards","PersistedDashBdDef")=>("",&[("dashboardIdPrefix","DefinitionID")]),
        ("kinetic-applications-and-layers","KineticApplicationBundle")=>("",&[("applicationIdPrefix","ViewId")]),
        ("kinetic-personalizations","PersistedXXXDef"|"PersistedKineticPersonalization"|"PersistedShellLayout"|"PersistedShellLayoutPersonal"|"PersistedHomeUserOption"|"PersistedFavFolder"|"PersistedFavItems")=>("",&[("userIdPrefix","$personalOwner")]),
        ("searches-and-navigation","PersistedQuickSearch")=>("$quickOrPredictive",&[("searchIdPrefix","QuickSearchID")]),
        ("searches-and-navigation","PersistedNamedSearch")=>("namedSearch",&[("searchIdPrefix","NSId")]),
        ("searches-and-navigation","PersistedESSearch")=>("enterpriseSearch",&[("searchIdPrefix","SearchID")]),
        ("searches-and-navigation","PersistedContextMenuXml")=>("savedContextMenu",&[]),
        ("searches-and-navigation","PersistedContextMenu")=>("legacyContextMenu",&[]),
        ("menus-and-launch-definitions","PersistedMenu")=>("",&[("menuIdPrefix","MenuID")]),
        ("report-definitions","ReportDataDefinition")=>("reportData",&[("definitionIdPrefix","RptDefID")]),
        ("report-definitions","BAQReport")=>("baqReports",&[("definitionIdPrefix","BAQRptID")]),
        ("report-styles-and-layouts","ReportStyle")=>("",&[("reportIdPrefix","ReportID")]),
        ("report-styles-and-layouts","CurrentCustomReportPackage"|"PersistedReportHistory")=>("",&[("packagePathPrefix","PrintProgram")]),
        ("report-behavior","ReportBehavior")=>("",&[("reportIdPrefix","ReportID")]),
        ("configurators","Configurator")=>("configurators",&[("definitionIdPrefix","ConfigID")]),
        ("configurators","ConfiguratorLookup")=>("lookups",&[("definitionIdPrefix","LookupTblID")]),
        ("configurators","PersistedPcInputVar"|"PersistedPcMethodVar"|"PersistedPcRuleSet"|"PersistedPcRules"|"PersistedPcRulesExpr")=>("persistedSources",&[]),
        ("business-rules-and-mappings","BPMDataForm")=>("forms",&[("definitionIdPrefix","FormId")]),
        ("business-rules-and-mappings","StructuredOutput")=>("structuredOutput",&[("definitionIdPrefix","RptStructuredOutputDefID")]),
        ("business-rules-and-mappings","PostingRules")=>("postingRules",&[]),
        ("programs-and-extensions","ServiceDefinition")=>("services",&[("definitionIdPrefix","ClassName")]),
        ("programs-and-extensions","DatasetDefinition")=>("datasets",&[("definitionIdPrefix","DataSetID")]),
        ("programs-and-extensions","ExtensionSet")=>("extensions",&[("definitionIdPrefix","ExtensionSetID")]),
        ("programs-and-extensions","ExternalAssemblyReference")=>("externals",&[("definitionIdPrefix","Name")]),
        ("solutions-and-packages","PersistedSolutionHeader"|"RetainedInstalledSolution")=>(if id.kind=="PersistedSolutionHeader"{"solutions"}else{"retainedPackages"},&[("solutionIdPrefix","SolutionID")]),
        ("solutions-and-packages","PersistedSolutionType")=>("solutionTypes",&[]),
        ("automation-and-integrations","SystemAgent")=>("systemAgents",&[]),
        ("automation-and-integrations","ProcessSet")=>("processSets",&[]),
        ("automation-and-integrations","MessageTemplate")=>("messageTemplates",&[]),
        ("automation-and-integrations","GlobalAlert")=>("globalAlerts",&[]),
        ("access-and-assignments","UserAccessAssignment"|"UserCompanyAssignment"|"UserExternalCompanyAssignment")=>("userAssignments",&[("userIdPrefix","UserID")]),
        ("access-and-assignments","APIKeyScopeAssignment")=>("apiKeyAssignments",&[]),
        ("access-and-assignments","SecurityDefinition"|"ObjectSecurity"|"SecurityGroup"|"ColumnSecurity"|"AccessScope"|"NativeSecuritySourceSupplement"|"NativeAccessSupplementary")=>("definitions",&[]),
        _=>return false,
    };
    selection.iter().all(|(setting,rule)|match rule {
        Selection::Members{values}=>if family=="$quickOrPredictive" {values.contains("quickSearch")&&values.contains("predictiveSearch")}else{values.contains(family)},
        Selection::LiteralPrefix{value,..}=>match prefixes.iter().find(|(key,_)|*key==setting) {
            Some((_,"$personalOwner"))=>match &id.scope {herbert_contract::Scope::Personal{owner,..}=>owner.starts_with(value),_=>value.is_empty()},
            Some((_,native_key))=>id.keys.get(*native_key).is_some_and(|id|id.starts_with(value)),
            // A reviewed independent root is unaffected by this category's
            // other discovery prefix (UD mappings, stored packages, etc.).
            None=>true,
        },
        _=>false,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(crate) struct Capability {
    /// Semantic identity, including relevant declaration and selected package
    /// identities. A supplied preview digest is not the source of these facts.
    pub binding: String,
    pub selection: BTreeMap<String, Selection>,
    /// Stable effect keys describe helper operations, native export files or
    /// exact eligible group remedies. Values contain only material scope facts.
    pub effects: BTreeMap<String, Value>,
}
impl Capability {
    fn valid_target_context(&self,company:&str)->bool {
        self.selection.values().all(|selection|match selection {
            Selection::NativeTargets{targets:Targets::Exact{values},..}=>values.iter().all(|text|serde_json::from_str::<herbert_contract::Identity>(text).ok().is_some_and(|id|crate::application::identity_in_scope(&id,company))),
            _=>true,
        })
    }
    fn contains(&self, requested: &Self) -> bool {
        self.binding == requested.binding
            && self.selection.len() == requested.selection.len()
            && requested.selection.iter().all(|(key, value)| self.selection.get(key).is_some_and(|granted| granted.contains(value)))
            && requested.effects.iter().all(|(key, value)| self.effects.get(key) == Some(value))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(crate) struct MaterialScope {
    pub endpoint: String,
    pub company: String,
    pub principal: String,
    pub capabilities: BTreeMap<String, Capability>,
}
impl MaterialScope {
    pub fn contains(&self, requested: &Self) -> bool {
        self.endpoint == requested.endpoint && self.company == requested.company && self.principal == requested.principal
            && requested.capabilities.iter().all(|(key, value)| value.valid_target_context(&self.company) && self.capabilities.get(key).is_some_and(|granted| granted.contains(value)))
    }
    pub fn has_effects(&self) -> bool { self.capabilities.values().any(|c| !c.effects.is_empty()) }
    pub fn digest(&self) -> String {
        // Canonical object order is explicit, independent of serde_json's map
        // implementation. Arrays are retained; planners sort semantic sets.
        fn canonical(value: Value) -> Value {
            match value {
                Value::Object(values) => json!(values.into_iter().map(|(k,v)|(k,canonical(v))).collect::<BTreeMap<_,_>>()),
                Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
                value => value,
            }
        }
        storage::sha256(&serde_json::to_vec(&canonical(json!(self))).expect("Material facts are JSON"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag="kind", rename_all="camelCase", rename_all_fields="camelCase", deny_unknown_fields)]
pub(crate) enum Lifetime {
    OneRun { request_id: String },
    Session { session_id: String },
    Workflow { workflow_id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(crate) struct Grant {
    pub id: String,
    pub scope: MaterialScope,
    pub digest: String,
    pub lifetime: Lifetime,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub revoked_at: Option<u64>,
    /// Interactive GUI action or explicit local-client acknowledgement. Neither
    /// a client name nor a digest proves a human read the preview.
    pub origin: String,
}

#[derive(Clone, Copy)]
pub(crate) struct Use<'a> {
    pub request_id: &'a str,
    pub session_id: &'a str,
    pub workflow_id: Option<&'a str>,
}
impl Grant {
    pub fn permits(&self, requested: &MaterialScope, use_: &Use<'_>) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expiry| now() < expiry)
            && self.digest == self.scope.digest() && self.scope.contains(requested)
            && match &self.lifetime {
                Lifetime::OneRun { request_id } => !request_id.is_empty() && request_id == use_.request_id,
                Lifetime::Session { session_id } => !session_id.is_empty() && session_id == use_.session_id,
                Lifetime::Workflow { workflow_id } => !workflow_id.is_empty() && Some(workflow_id.as_str()) == use_.workflow_id,
            }
    }
    /// Revocation/expiry/session end cannot abandon already-owned resources.
    /// Callers must independently prove the original operation/grant binding and
    /// exact native resource ownership before using this cleanup-only authority.
    pub fn permits_owned_cleanup(&self, original_scope: &MaterialScope) -> bool {
        original_scope.has_effects() && self.digest == self.scope.digest() && self.scope.contains(original_scope)
    }
}

/// Attribution for one durable logical run. This record does not own resources;
/// the existing artifact/access/transfer/lease records retain that authority.
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct RunAuthority {
    pub grant_id:String,
    pub material_scope:MaterialScope,
    pub material_digest:String,
    pub request_id:String,
    pub original_session:String,
    pub workflow_id:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct AuthorizationRef {pub grant_id:String,pub material_digest:String,pub request_id:String}
impl AuthorizationRef {
    pub fn matches(&self,authority:&RunAuthority)->bool {self.grant_id==authority.grant_id&&self.material_digest==authority.material_digest&&self.request_id==authority.request_id}
}
impl RunAuthority {
    pub fn reference(&self)->AuthorizationRef {AuthorizationRef{grant_id:self.grant_id.clone(),material_digest:self.material_digest.clone(),request_id:self.request_id.clone()}}
    pub fn check(&self,root:&Path,current_session:&str)->Result<Grant,Diagnostic> {
        let grant=get(root,&self.grant_id)?;
        if self.material_digest!=self.material_scope.digest() || !grant.permits(&self.material_scope,&Use{request_id:&self.request_id,session_id:current_session,workflow_id:self.workflow_id.as_deref()}) {
            return Err(refusal("This run’s server-change authorization is revoked, expired, outside scope or belongs to another session/workflow. Direct collection without temporary access remains available where supported."));
        }
        Ok(grant)
    }
    pub fn cleanup_grant(&self,root:&Path)->Result<Grant,Diagnostic> {
        let grant=get(root,&self.grant_id)?;
        if self.material_digest!=self.material_scope.digest() || !grant.permits_owned_cleanup(&self.material_scope) {return Err(refusal("The original cleanup authorization no longer binds this run’s recorded material scope. Preserve its ownership evidence."));}
        Ok(grant)
    }
}

fn state(root: &Path) -> StateFile { StateFile::at(root.join("server-change-authorizations.json")) }
fn read(root: &Path) -> Result<Value, Diagnostic> {
    let value = state(root).read()?;
    if value.is_null() { return Ok(json!({"schemaVersion":1,"grants":{}})); }
    if value["schemaVersion"] != 1 || !value["grants"].is_object() { return Err(refusal("The saved server-change authority is invalid. It was preserved.")); }
    Ok(value)
}
pub(crate) fn list(root: &Path) -> Result<Vec<Grant>, Diagnostic> {
    read(root)?["grants"].as_object().expect("checked object").values().map(|v|serde_json::from_value(v.clone()).map_err(|_| refusal("A saved authorization record is invalid. It was preserved."))).collect()
}
pub(crate) fn get(root: &Path, id: &str) -> Result<Grant, Diagnostic> {
    list(root)?.into_iter().find(|g|g.id == id).ok_or_else(||refusal("The referenced server-change authorization does not exist."))
}
/// Called only after the host consumes its issued preview on the separate
/// explicit authorization route. Ordinary starts/preferences never call this.
pub(crate) fn create(root: &Path, scope: MaterialScope, lifetime: Lifetime, expires_at: Option<u64>, origin: &str) -> Result<Grant, Diagnostic> {
    if !scope.has_effects() { return Err(refusal("This preview needs no server-change authorization.")); }
    let valid_lifetime = match &lifetime { Lifetime::OneRun {request_id} => !request_id.is_empty(), Lifetime::Session {session_id} => !session_id.is_empty(), Lifetime::Workflow {workflow_id} => !workflow_id.is_empty() };
    if !valid_lifetime || expires_at.is_some_and(|expiry| expiry <= now()) { return Err(refusal("Choose a defined authorization lifetime and, when supplied, a future expiry.")); }
    if !matches!(origin, "interactiveGui" | "explicitLocalClient") { return Err(refusal("The authorization creation route is not identified.")); }
    std::fs::create_dir_all(root).map_err(|_|refusal("Cannot open the host authorization directory."))?;
    let original = state(root).read()?;
    let mut after = read(root)?;
    let grant = Grant {id:uuid::Uuid::new_v4().to_string(),digest:scope.digest(),scope,lifetime,created_at:now(),expires_at,revoked_at:None,origin:origin.into()};
    after["grants"][&grant.id] = json!(grant);
    state(root).replace(&original, &after)?;
    Ok(grant)
}
pub(crate) fn revoke(root: &Path, id: &str) -> Result<Grant, Diagnostic> {
    let original = state(root).read()?;
    let mut after = read(root)?;
    let mut grant = get(root,id)?;
    if grant.revoked_at.is_none() { grant.revoked_at = Some(now()); after["grants"][id] = json!(grant); state(root).replace(&original,&after)?; }
    Ok(grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(prefix:&str) -> MaterialScope {
        MaterialScope {endpoint:"https://test/".into(),company:"C".into(),principal:"user".into(),capabilities:BTreeMap::from([("functions".into(),Capability{binding:"reader-package-and-declaration".into(),selection:BTreeMap::from([("library".into(),Selection::LiteralPrefix{value:prefix.into(),contract:"literal-case-sensitive-v1".into()}),("compiled".into(),Selection::Included{value:true})]),effects:BTreeMap::from([("helper".into(),json!({"type":"functionLibrary","package":"hash"}))])})] )}
    }
    #[test]
    fn narrower_material_scope_reuses_authority_but_expansion_and_package_change_do_not() {
        let full=scope("Ab"); let mut narrow=scope("Ab Ω");
        assert!(full.contains(&narrow));
        assert!(!full.contains(&scope("ab")));
        assert!(!full.contains(&scope("A")));
        narrow.capabilities.get_mut("functions").unwrap().selection.insert("compiled".into(),Selection::Included{value:false});
        assert!(full.contains(&narrow));
        narrow.capabilities.get_mut("functions").unwrap().binding="new-package".into();
        assert!(!full.contains(&narrow));
        let mut changed=scope("Ab"); changed.company="other".into(); assert!(!full.contains(&changed));
        changed=scope("Ab"); changed.principal="other".into(); assert!(!full.contains(&changed));
        assert_eq!(full.digest(),scope("Ab").digest());
    }
    #[test]
    fn lifetime_and_revocation_block_new_uses_without_removing_owned_cleanup_authority() {
        let t=tempfile::tempdir().unwrap(); let s=scope("Ab");
        let g=create(t.path(),s.clone(),Lifetime::OneRun{request_id:"request".into()},None,"explicitLocalClient").unwrap();
        let use_=Use{request_id:"request",session_id:"session",workflow_id:None};
        assert!(g.permits(&s,&use_));
        assert!(!g.permits(&s,&Use{request_id:"another",..use_}));
        let revoked=revoke(t.path(),&g.id).unwrap(); assert!(!revoked.permits(&s,&use_)); assert!(revoked.permits_owned_cleanup(&s));
        let session=create(t.path(),s.clone(),Lifetime::Session{session_id:"session".into()},None,"interactiveGui").unwrap();
        assert!(session.permits(&s,&use_)); assert!(!session.permits(&s,&Use{session_id:"restart",..use_}));
        let workflow=create(t.path(),s.clone(),Lifetime::Workflow{workflow_id:"nightly".into()},None,"interactiveGui").unwrap();
        assert!(!workflow.permits(&s,&use_)); assert!(workflow.permits(&s,&Use{workflow_id:Some("nightly"),..use_}));
        assert_eq!(list(t.path()).unwrap().len(),3);
    }
}
