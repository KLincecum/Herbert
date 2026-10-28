//! Host-issued read-only plans and the explicit authorization control surface.
//! This module never installs/invokes helpers, borrows access, exports files or
//! reconciles native resources to obtain a preview.
use super::*;
use crate::server_changes::{self, Capability, Lifetime, MaterialScope, Selection};
use std::collections::BTreeSet;

#[derive(Clone)]
pub(super) struct IssuedPreview {
    pub scope: MaterialScope,
    pub session: String,
    pub request_id: String,
    pub full: bool,
    pub run: Value,
    pub unresolved: Vec<Diagnostic>,
    pub created: Option<(AuthorizeInput,String)>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(super) struct PreviewInput {
    pub full_backup: bool,
    pub run: StartInput,
}

#[derive(Clone,PartialEq,Deserialize,schemars::JsonSchema)]
#[serde(tag="kind", rename_all="camelCase", rename_all_fields="camelCase", deny_unknown_fields)]
pub(super) enum RequestedLifetime {
    OneRun,
    Session,
    Workflow { workflow_id: String },
}
#[derive(Clone,PartialEq,Deserialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub(super) struct AuthorizeInput {
    pub preview_id: String,
    pub material_digest: String,
    pub lifetime: RequestedLifetime,
    pub expires_at: Option<u64>,
}

#[derive(Serialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase")]
pub(super) struct AuthorizationResult {pub authorization:server_changes::Grant,pub reconciled:bool}
#[derive(Serialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase")]
pub(super) struct AuthorizationList {pub authorizations:Vec<server_changes::Grant>}
#[derive(Serialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase")]
pub(super) struct PreviewResult {
    pub preview_id:String,pub material_digest:String,pub material_scope:MaterialScope,pub display:Value,
    pub unresolved:Vec<Diagnostic>,pub authorization_required:bool,pub read_only:bool,
    pub matching_authorizations:Vec<server_changes::Grant>,
}

fn issue(message: &str) -> Diagnostic { Diagnostic::new("SERVER_CHANGE_PREVIEW_REQUIRED", message) }
fn matching_contract(description: &str) -> Option<&str> {
    description.split_once("Matching contract:").and_then(|(_,v)|v.split_whitespace().next()).map(|v|v.trim_end_matches('.'))
}
// Reviewed owning contracts: category and setting key are part of the rule.
// A familiar label or matching-contract suffix grants no semantics to a plugin.
fn prefix_contract(category: &str, key: &str) -> Option<&'static str> {
    Some(match (category,key) {
        ("C01","codeTypeIdPrefix")=>"c01-code-type-id-prefix-v1",
        ("C02","schemaTableIdPrefix")=>"c02-schema-table-id-prefix-v1",
        ("C03","tableIdPrefix")=>"c03-table-id-prefix-v1",
        ("C04","queryIdPrefix")=>"c04-query-id-prefix-v1",
        ("C06","methodCodePrefix")=>"c06-method-code-prefix-v1",
        ("C08","libraryIdPrefix")=>"c08-library-id-prefix-v1",
        ("C09","layerNamePrefix")=>"c09-layer-name-prefix-v1",
        ("C09","formNamePrefix")=>"c09-form-name-prefix-v1",
        ("C10","userIdPrefix")=>"c10-user-id-prefix-v1",
        ("C10","formNamePrefix")=>"c10-form-name-prefix-v1",
        ("C11","dashboardIdPrefix")=>"c11-dashboard-id-prefix-v1",
        ("C12","applicationIdPrefix")=>"c12-application-id-prefix-v1",
        ("C13","userIdPrefix")=>"c13-user-id-prefix-v1",
        ("C14","searchIdPrefix")=>"c14-search-id-prefix-v1",
        ("C15","menuIdPrefix")=>"c15-menu-id-prefix-v1",
        ("C16","definitionIdPrefix")=>"c16-definition-id-prefix-v1",
        ("C17","reportIdPrefix")=>"c17-report-id-prefix-v1",
        ("C17","packagePathPrefix")=>"c17-package-path-prefix-v1",
        ("C18","reportIdPrefix")=>"c18-report-id-prefix-v1",
        ("C19","definitionIdPrefix")=>"c19-definition-id-prefix-v1",
        ("C20","definitionIdPrefix")=>"c20-definition-id-prefix-v1",
        ("C22","definitionIdPrefix")=>"c22-definition-id-prefix-v1",
        ("C24","solutionIdPrefix")=>"c24-solution-id-prefix-v1",
        ("C26","userIdPrefix")=>"c26-user-id-prefix-v1",
        _=>return None,
    })
}
fn family_contract(category: &str,key: &str) -> Option<(&'static str,&'static str,&'static [&'static str])> {
    Some(match (category,key) {
        ("C02","definitionFamily")=>("c02-definition-family-v1","schemaAndAllMappings", &["schema","mappings"]),
        ("C06","directiveFamily")=>("c06-directive-family-v1","methodAndData", &["method","data"]),
        ("C14","definitionFamily")=>("c14-definition-family-v1","all", &["quickSearch","predictiveSearch","namedSearch","enterpriseSearch","savedContextMenu","legacyContextMenu"]),
        ("C16","definitionFamily")=>("c16-definition-family-v1","reportDataAndBaqReports", &["reportData","baqReports"]),
        ("C19","definitionFamily")=>("c19-definition-family-v1","all", &["configurators","lookups","persistedSources"]),
        ("C20","definitionFamily")=>("c20-definition-family-v1","all", &["forms","structuredOutput","postingRules"]),
        ("C21","resourceFamily")=>("c21-resource-family-v1","all", &["themes","translations","images","fileStore"]),
        ("C22","definitionFamily")=>("c22-definition-family-v1","all", &["services","datasets","extensions","externals"]),
        ("C24","definitionFamily")=>("c24-definition-family-v1","all", &["solutions","solutionTypes","retainedPackages"]),
        ("C25","automationFamily")=>("c25-automation-family-v1","all", &["systemAgents","processSets","messageTemplates","globalAlerts"]),
        ("C26","accessFamily")=>("c26-access-family-v1","all", &["definitions","userAssignments","apiKeyAssignments"]),
        _=>return None,
    })
}
pub(super) fn tuning(category: &str, declaration: &PluginSetting) -> bool {
    // C13 qualification proves byte-identical source files across these two
    // acquisition routes. This exact declaration is transport tuning only.
    if category=="C13" && matches!(declaration,PluginSetting::Checkbox{key,label,description,..} if key=="combinedRead" && label=="Combine source reads in one BAQ" && description=="Read personalization source groups together with an unsaved BAQ. File dependencies still use their native readers. Your choice is saved.") {return true;}
    matches!(declaration,PluginSetting::Integer{..}) &&
        ((declaration.key()=="itemsPerPage" && matches!(category,"C01"|"C02"|"C06"|"C08"|"C09"|"C10"|"C11"|"C12"|"C13"|"C14"|"C15"|"C16"|"C17"|"C18"|"C19"|"C20"))
         || (category=="C04" && declaration.key()=="queriesPerPage")
         || (category=="C12" && declaration.key()=="metadataRowsPerPage"))
}
fn included_checkbox(category: &str,key: &str,description: &str) -> Option<bool> {
    // Exact owning descriptions protect same-name controls with unreviewed
    // meanings. The module bytes and declaration are also in the binding.
    let expected=match (category,key) {
        ("C08","downloadCompiledAssemblies")=>"Preserve associated original DLLs and available symbols under Herbert Data/raw. Keep the whole backup and journal. Exact store access requires the optional scoped reader.",
        ("C08","decompileCompiledAssemblies")=>"Also capture compiled assemblies. Reconstruct local C# projects. References are not downloaded. Reconstructed source may be partial and may not build.",
        ("C06","downloadCompiledAssemblies")=>"Preserve associated original DLL/PDB bytes under Herbert Data/raw. Keep the whole backup and its journal. Capture requires the scoped read helper. Normal BPM output remains independent.",
        ("C06","decompileCompiledAssemblies")=>"Also capture original assemblies. Create local projects under their BPM owners. Data-trigger projects contain both directive types. Missing dependencies and reconstruction limits remain explicit.",
        ("C04","downloadCompiledAssemblies")=>"Preserve original DLL/PDB bytes under Herbert Data/raw with exact query and method associations. Keep the whole backup and journal. Capture requires a read-only helper. Normal BAQ output remains independent.",
        ("C04","decompileCompiledAssemblies")=>"Also capture original assemblies. Create local projects under each UBAQ method. References are not downloaded. Reconstructed source may be partial and may not build. Native recovery uses the BAQ package.",
        ("C12","includeVersionHistory")=>"Collect older saved revisions. Turn this off for current published definitions and saved drafts only. Your choice is saved with this plugin.",
        ("C11","includeReferencedBAQs")=>"Include native BAQ exports in each DBD, as Classic dashboard export does. Disable for definition-only files.",
        ("C25","filterSensitiveFields")=>"Off by default: preserve native field and parameter values. When enabled, omit declared password, secret, API key and token values from this plugin’s collected definitions. System artifact exclusions still apply.",
        _=>return None,
    };
    (description==expected).then_some(key=="filterSensitiveFields")
}
fn selection(category: &str, declarations: &[PluginSetting], values: &Value) -> BTreeMap<String,Selection> {
    let mut selected=BTreeMap::new();
    for declaration in declarations {
        if declaration.demo_only() || tuning(category,declaration) {continue;}
        let key=declaration.key();let value=values[key].clone();
        let rule=match declaration {
            PluginSetting::Filter{description,..} if prefix_contract(category,key).is_some_and(|c|matching_contract(description)==Some(c)) =>
                value.as_str().map(|s|Selection::LiteralPrefix{value:s.into(),contract:prefix_contract(category,key).unwrap().into()}).unwrap_or(Selection::Exact{value}),
            PluginSetting::Choice{description,options,..} => {
                if let Some((contract,combined,members))=family_contract(category,key) {
                    let expected=members.iter().copied().chain(std::iter::once(combined)).collect::<BTreeSet<_>>();
                    let actual=options.iter().filter_map(|o|o.value.as_str()).collect::<BTreeSet<_>>();
                    if matching_contract(description)==Some(contract) && actual==expected && options.len()==expected.len() {
                        match value.as_str() {
                            Some(v) if v==combined=>Selection::Members{values:members.iter().map(|v|(*v).into()).collect()},
                            Some(v) if members.contains(&v)=>Selection::Members{values:BTreeSet::from([v.into()])},
                            _=>Selection::Exact{value},
                        }
                    } else {Selection::Exact{value}}
                } else {Selection::Exact{value}}
            }
            PluginSetting::Checkbox{description,..} => match included_checkbox(category,key,description) {
                Some(inverse) if value.is_boolean()=>Selection::Included{value:if inverse {value!=true}else{value==true}},
                _=>Selection::Exact{value},
            },
            _=>Selection::Exact{value},
        };
        selected.insert(key.into(),rule);
    }
    selected
}
pub(super) fn semantic_binding(category: &str,module_hash: &str,declarations: &[PluginSetting]) -> String {
    let metadata=declarations.iter().filter(|d|!d.demo_only()&&!tuning(category,d)).map(|d| {
        let mut value=json!(d);value.as_object_mut().expect("setting object").remove("default");value
    }).collect::<Vec<_>>();
    storage::sha256(&serde_json::to_vec(&json!({"category":category,"moduleSha256":module_hash,"selectionSemantics":"reviewed-native-targets-v2","declarations":metadata})).expect("semantic binding JSON"))
}
fn content_selection(category:&str,declarations:&[PluginSetting],values:&Value,identities:Option<&[Identity]>) -> BTreeMap<String,Selection> {
    let mut content=BTreeMap::new();let mut discovery=BTreeMap::new();
    for (key,value) in selection(category,declarations,values) {
        if matches!(&value,Selection::LiteralPrefix{..}|Selection::Members{..}) {discovery.insert(key,value);}else{content.insert(key,value);}
    }
    content.insert("$nativeTargets".into(),Selection::NativeTargets{category:category.into(),targets:match identities {
        Some(ids)=>server_changes::Targets::Exact{values:ids.iter().filter(|i|i.category==category).map(|i|serde_json::to_string(i).expect("native identity JSON")).collect()},
        None=>server_changes::Targets::Discovery{selection:discovery},
    }});
    content
}
fn needed(alias: &str, settings: &Value) -> bool {
    match alias {
        "compiled_functions"|"compiled_bpm"|"compiled_ubaq" => settings["downloadCompiledAssemblies"]==true || settings["decompileCompiledAssemblies"]==true,
        "history" => settings.get("includeVersionHistory").is_none_or(|v|v==true),
        _ => true,
    }
}

fn native_file_effect_selected(category:&str,service:&str,method:&str,declarations:&[PluginSetting],values:&Value,full:bool,identities:&[Identity])->bool {
    if !matches!((service,method),("Ice.BO.DynamicReportSvc","ExportToFile")|("Erp.BO.ConfiguratorDefSvc","ExportConfiguration")){return false;}
    let family=match (category,service,method) {
        ("C16","Ice.BO.DynamicReportSvc","ExportToFile")=>Some(("baqReports","BAQReport")),
        ("C19","Erp.BO.ConfiguratorDefSvc","ExportConfiguration")=>Some(("configurators","Configurator")),
        _=>None,
    };
    if let Some((member,kind))=family {
        if !full {return identities.iter().any(|id|id.category==category&&id.kind==kind);}
        // Only the reviewed family contract can exclude an effect. A familiar
        // unreviewed choice label cannot make a possible write disappear.
        if let Some(Selection::Members{values})=selection(category,declarations,values).get("definitionFamily") {return values.contains(member);}
    }
    true
}

impl Host {
    pub(super) async fn plan_server_changes(&self, input:&StartInput, full:bool) -> Result<(MaterialScope,Value,Vec<Diagnostic>),Diagnostic> {
        let connection=self.current_connection().await?;
        let (installed,discovery)=self.installed().await?;
        let available=installed.iter().filter(|p|p.category.available).map(|p|p.category.id.clone()).collect::<BTreeSet<_>>();
        let selected=if full {select_full_categories(&available,input.categories.clone(),&discovery)?} else {
            if input.identities.is_empty(){return Err(issue("Exact retrieval preview needs its selected native identities."));}
            let ids=input.identities.iter().map(|i|i.category.clone()).collect::<BTreeSet<_>>();
            if !ids.is_subset(&available){return Err(issue("A selected category is unavailable."));}ids.into_iter().collect()
        };
        let method=input.collection_method.unwrap_or(CollectionMethod::DirectOnly);
        let caller=connection.caller().await.ok_or_else(||issue("Verify the native initiating caller before preview."))?;
        let mut scope=MaterialScope {endpoint:connection.endpoint.to_string(),company:connection.company.clone(),principal:caller,capabilities:BTreeMap::new()};
        let version=connection.server_version().await.unwrap_or_default();
        let mut unresolved=Vec::new();let mut display=Vec::new();let mut policy_methods:BTreeMap<String,BTreeSet<String>>=BTreeMap::new();
        let mut needs_host=false;
        let legacy_selected=method==CollectionMethod::ServerAssisted && installed.iter().any(|p|selected.contains(&p.category.id)&&!p.category.helpers.is_empty());
        let legacy_version=if legacy_selected {
            match connection.helper_metadata("GetDefaults",json!({}),&CancellationToken::new()).await {
                Ok(response)=>match response.value.pointer("/returnObj/EfxLibrary/0/EpicorVersion").and_then(Value::as_str) {
                    Some(version)=>Some(version.to_owned()),
                    None=>{unresolved.push(issue("Native defaults did not identify the legacy helper target version."));None}
                },
                Err(error)=>{unresolved.push(error);None}
            }
        }else{None};
        for plugin in installed.iter().filter(|p|selected.contains(&p.category.id)) {
            let values=resolve_plugin_settings(&plugin.category.settings,input.plugin_settings.get(&plugin.category.id).unwrap_or(&Value::Null))?;
            let mut predicates=content_selection(&plugin.category.id,&plugin.category.settings,&values,(!full).then_some(input.identities.as_slice()));
            predicates.insert("$moduleSha256".into(),Selection::Exact{value:json!(plugin.hash)});
            let mut capability=Capability {binding:semantic_binding(&plugin.category.id,&plugin.hash,&plugin.category.settings),selection:predicates,effects:BTreeMap::new()};
            let mut helpers=Vec::new();
            match plugin.snapshot_artifacts().await {
                Err(error)=>unresolved.push(error),
                Ok(snapshots)=>for snapshot in snapshots {
                    let declaration=&snapshot.declaration;
                    if !needed(&declaration.alias,&values){continue;}
                    let source=declaration.file_for_version(&version);
                    let Some(bytes)=snapshot.files.get(source) else {unresolved.push(issue("A selected helper package has no frozen source bytes."));continue;};
                    let effect=json!({"kind":"helper","declaration":declaration,"packageFile":source,"packageSha256":storage::sha256(bytes),"condition":"Only when selected native content requires the declared action","cleanup":"Existing owned artifact/lease lifecycle"});
                    helpers.push(effect.clone());
                    if method==CollectionMethod::ServerAssisted {
                        capability.effects.insert(format!("helper:{}",declaration.alias),effect);
                        needs_host|=declaration.artifact_type==NativeArtifactType::FunctionLibrary && declaration.lifetime==NativeArtifactLifetime::Temporary && declaration.target.is_none();
                    }
                }
            }
            for helper in &plugin.category.helpers {
                let mut contract=json!(helper);
                contract.as_object_mut().expect("helper contract object").remove("installation");
                let package=legacy_version.as_ref().and_then(|version|helper.installation.as_ref().and_then(|installation| {
                    let candidates=installation.packages.iter().filter(|p|&p.epicor_version==version).collect::<Vec<_>>();
                    (candidates.len()==1).then(||&candidates[0].package)
                }));
                let package_hash=package.map(|value|storage::sha256(&serde_json::to_vec(value).expect("helper package JSON")));
                let effect=json!({"kind":"legacyHelper","contract":contract,"nativeVersion":legacy_version,"packageSha256":package_hash,"condition":"Only when selected native content calls this declared helper","cleanup":"Existing owned helper-library lifecycle"});
                helpers.push(effect.clone());
                if method==CollectionMethod::ServerAssisted {
                    if package.is_some(){capability.effects.insert(format!("legacyHelper:{}",helper.library_id),effect);needs_host=true;}
                    else{unresolved.push(issue("A selected legacy helper has no unique package for the native target version."));}
                }
            }
            for (service,native_method) in &plugin.methods {
                if input.temporary_access {policy_methods.entry(service.clone()).or_default().insert(native_method.clone());}
                if native_file_effect_selected(&plugin.category.id,service,native_method,&plugin.category.settings,&values,full,&input.identities) {
                    capability.effects.insert(format!("nativeFile:{service}/{native_method}"),json!({"kind":"nativeExportFiles","service":service,"method":native_method,"folder":4,"ownership":"Unpredictable run-owned filename and exact native readback","cleanup":"Download/verify then native FileDelete/FileExists; preserve uncertain completion"}));
                }
            }
            display.push(json!({"category":plugin.category.id,"name":plugin.category.name,"effectiveSettings":values,"helpers":helpers,"helperPermission":method==CollectionMethod::ServerAssisted,"nativeFileEffects":capability.effects.values().filter(|v|v["kind"]=="nativeExportFiles").collect::<Vec<_>>() }));
            scope.capabilities.insert(plugin.category.id.clone(),capability);
        }
        if needs_host {
            match host_utility::snapshot_for_version(&version) {
                Ok(snapshot)=>{
                    let source=snapshot.declaration.file_for_version(&version);
                    if let Some(bytes)=snapshot.files.get(source) {
                        scope.capabilities.insert("$herbert-host".into(),Capability {binding:"host-utility-and-lease-v1".into(),selection:BTreeMap::new(),effects:BTreeMap::from([("helper:host".into(),json!({"kind":"hostHelper","declaration":snapshot.declaration,"packageFile":source,"packageSha256":storage::sha256(bytes),"effects":"Owned staging/transfer/ZIP and lease registry actions; exact existing lifecycle cleanup"}))])});
                    }else{unresolved.push(issue("The host utility has no source bytes for this native version."));}
                }
                Err(error)=>unresolved.push(error),
            }
        }
        if input.temporary_access {
            if needs_host {for method in ["GetDefaults","IsValidLibraryId","ImportLibrary","ApplyChangesWithDiagnostics","PromoteToProduction","GetLibrary","ExportLibrary"]{policy_methods.entry("Ice.Lib.EfxLibraryDesignerSvc".into()).or_default().insert(method.into());}}
            match self.preview_policy_effects(&connection,&policy_methods,&CancellationToken::new()).await {
                Ok(effects)=>{scope.capabilities.insert("$native-access".into(),Capability {binding:"stored-native-policy-remedy-v1".into(),selection:BTreeMap::new(),effects});}
                Err(error)=>unresolved.push(error),
            }
        }
        let presentation=json!({"endpoint":scope.endpoint,"company":scope.company,"principal":scope.principal,"destination":input.destination,"collectionMethod":method,"temporaryAccess":input.temporary_access,"capabilities":display,"possibleServerChanges":scope.has_effects(),"priorCleanup":"Not executed by preview. Existing unresolved ownership requires separately attributed authority and inspection.","trust":"This is a host-produced read-only plan. A client acknowledgement does not attest that a human read it."});
        Ok((scope,presentation,unresolved))
    }

    pub(super) async fn server_change_preview(&self, input:PreviewInput) -> Result<Value,Diagnostic> {
        let start=input.run;
        if start.request_id.is_empty(){return Err(issue("Supply the logical run requestId before preview."));}
        let (scope,display,unresolved)=self.plan_server_changes(&start,input.full_backup).await?;
        let id=uuid::Uuid::new_v4().to_string();let digest=scope.digest();
        let session=self.authorization_session.read().await.clone();
        let matching=server_changes::list(&self.state_root)?.into_iter().filter(|g|g.permits(&scope,&server_changes::Use{request_id:&start.request_id,session_id:&session,workflow_id:start.workflow_id.as_deref()})).collect();
        let issued=IssuedPreview {scope:scope.clone(),session,request_id:start.request_id.clone(),full:input.full_backup,run:json!(start),unresolved:unresolved.clone(),created:None};
        self.server_change_previews.lock().await.insert(id.clone(),issued);
        Ok(json!(PreviewResult{preview_id:id,material_digest:digest,authorization_required:scope.has_effects(),material_scope:scope,display,unresolved,read_only:true,matching_authorizations:matching}))
    }

    /// Only the GUI’s explicit confirmation button calls this route. Ordinary
    /// command clients use explicitLocalClient; neither route attests human reading.
    pub(crate) async fn authorize_interactive_server_changes(&self,input:Value)->Result<Value,Diagnostic> {
        let _operation=self.operations.read().await;
        if self.operation_cancel.is_cancelled(){return Err(issue("This host is shutting down."));}
        self.authorize_server_changes(decode(input)?,"interactiveGui").await
    }

    pub(super) async fn authorize_server_changes(&self,input:AuthorizeInput,origin:&str)->Result<Value,Diagnostic> {
        // Serialize consumption of an issued preview. Retrying the same explicit
        // acknowledgement returns its grant; it cannot mint a second grant or
        // silently replace the chosen lifetime.
        let mut previews=self.server_change_previews.lock().await;
        let issued=previews.get(&input.preview_id).cloned().ok_or_else(||issue("This host has no matching preview. Obtain a new read-only preview before authorizing changes."))?;
        if let Some((ack,id))=&issued.created {
            if ack!=&input {return Err(issue("This preview already created an authorization with another acknowledgement. Inspect it or obtain a new preview."));}
            return Ok(json!(AuthorizationResult{authorization:server_changes::get(&self.state_root,id)?,reconciled:true}));
        }
        if issued.session!=*self.authorization_session.read().await || input.material_digest!=issued.scope.digest() || !issued.unresolved.is_empty() {
            return Err(issue("The preview session/material facts are stale or unresolved. Obtain a new read-only preview."));
        }
        let start:StartInput=decode(issued.run.clone())?;
        let (fresh,_,unresolved)=self.plan_server_changes(&start,issued.full).await?;
        if !unresolved.is_empty()||fresh.digest()!=input.material_digest {return Err(issue("Material facts changed or remain unresolved. Review a new preview before creating authority."));}
        let lifetime=match &input.lifetime {
            RequestedLifetime::OneRun=>Lifetime::OneRun{request_id:issued.request_id.clone()},
            RequestedLifetime::Session=>Lifetime::Session{session_id:issued.session.clone()},
            RequestedLifetime::Workflow{workflow_id}=>Lifetime::Workflow{workflow_id:workflow_id.clone()},
        };
        let grant=server_changes::create(&self.state_root,fresh,lifetime,input.expires_at,origin)?;
        let stored=previews.get_mut(&input.preview_id).expect("held preview map");
        stored.created=Some((input,grant.id.clone()));
        Ok(json!(AuthorizationResult{authorization:grant,reconciled:false}))
    }

    /// Resolve fresh material facts for a new logical run. Stored preferences
    /// cannot grant anything; the caller must supply the explicit grant reference.
    pub(super) async fn admit_server_changes(&self,input:&StartInput,full:bool)->Result<Option<server_changes::RunAuthority>,Diagnostic> {
        let (scope,_,unresolved)=self.plan_server_changes(input,full).await?;
        if !unresolved.is_empty() {let mut error=issue("The possible server-change scope is unresolved. Review its diagnostics, or use Direct without temporary access for independent supported collection.");error.context.insert("unresolved".into(),json!(unresolved));return Err(error);}
        if !scope.has_effects(){return Ok(None);}
        let id=input.authorization_id.as_ref().ok_or_else(||issue("This selection may change the server. Use serverChanges.preview and explicitly authorize the defined effects, or use Direct without temporary access where supported."))?;
        let authority=server_changes::RunAuthority{grant_id:id.clone(),material_digest:scope.digest(),material_scope:scope,request_id:input.request_id.clone(),original_session:self.authorization_session.read().await.clone(),workflow_id:input.workflow_id.clone()};
        authority.check(&self.state_root,&authority.original_session)?;
        Ok(Some(authority))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    fn filter(key:&str,contract:&str)->PluginSetting {PluginSetting::filter(key,"ID","").description(&format!("Literal, case-sensitive prefix. Matching contract: {contract}."))}
    fn identity(category:&str,kind:&str,scope:herbert_contract::Scope,keys:&[(&str,&str)])->Identity {Identity{category:category.into(),kind:kind.into(),scope,keys:keys.iter().map(|(k,v)|((*k).into(),(*v).into())).collect()}}
    fn scope(category:&str,declarations:&[PluginSetting],values:Value,ids:Option<&[Identity]>,package:&str)->MaterialScope {
        MaterialScope{endpoint:"https://test/".into(),company:"C".into(),principal:"user".into(),capabilities:BTreeMap::from([(category.into(),Capability{binding:semantic_binding(category,package,declarations),selection:content_selection(category,declarations,&values,ids),effects:BTreeMap::new()})])}
    }
    #[test]
    fn known_contracts_do_not_extend_to_familiar_but_unreviewed_declarations() {
        let declarations=vec![filter("libraryIdPrefix","c08-library-id-prefix-v1"),filter("otherPrefix","c08-other-prefix-v1"),PluginSetting::checkbox("downloadCompiledAssemblies","Compiled",true).description("Delete originals when enabled"),PluginSetting::choice("definitionFamily","Family","all",[("All","all"),("One","one")].map(|(label,value)|herbert_contract::PluginSettingOption{label:label.into(),value:value.into()})).description("Matching contract: unreviewed-family-v1.")];
        let rules=selection("C08",&declarations,&json!({"libraryIdPrefix":" Ab ","otherPrefix":"A","downloadCompiledAssemblies":true,"definitionFamily":"all"}));
        assert!(matches!(&rules["libraryIdPrefix"],Selection::LiteralPrefix{value,..} if value==" Ab "));
        for key in ["otherPrefix","downloadCompiledAssemblies","definitionFamily"]{assert!(matches!(&rules[key],Selection::Exact{..}));}
        let wrong=selection("C99",&declarations,&json!({"libraryIdPrefix":"Ab"}));assert!(matches!(&wrong["libraryIdPrefix"],Selection::Exact{..}));
        let unknown_contract=vec![filter("libraryIdPrefix","c08-unreviewed-prefix-v1")];assert!(matches!(&selection("C08",&unknown_contract,&json!({"libraryIdPrefix":"A"}))["libraryIdPrefix"],Selection::Exact{..}));
    }
    #[test]
    fn full_discovery_covers_only_actual_exact_targets_and_ignores_saved_discovery_for_exact() {
        let d=vec![filter("libraryIdPrefix","c08-library-id-prefix-v1")];
        let inside=identity("C08","EfxLibrary",herbert_contract::Scope::Global,&[("LibraryID","AbOne")]);
        let outside=identity("C08","EfxLibrary",herbert_contract::Scope::Global,&[("LibraryID","Other")]);
        let exact=scope("C08",&d,json!({"libraryIdPrefix":"Ignored"}),Some(std::slice::from_ref(&inside)),"package");
        assert!(scope("C08",&d,json!({"libraryIdPrefix":""}),None,"package").contains(&exact));
        assert!(scope("C08",&d,json!({"libraryIdPrefix":"Ab"}),None,"package").contains(&exact));
        assert!(!scope("C08",&d,json!({"libraryIdPrefix":"Else"}),None,"package").contains(&exact));
        assert_eq!(exact.digest(),scope("C08",&d,json!({"libraryIdPrefix":"Changed saved discovery"}),Some(std::slice::from_ref(&inside)),"package").digest());
        let both=scope("C08",&d,json!({"libraryIdPrefix":""}),Some(&[inside.clone(),outside]),"package");assert!(both.contains(&exact));assert!(!exact.contains(&both));
        let mut unknown=inside.clone();unknown.kind="FutureRoot".into();assert!(!scope("C08",&d,json!({"libraryIdPrefix":""}),None,"package").contains(&scope("C08",&d,json!({"libraryIdPrefix":""}),Some(&[unknown]),"package")));
        assert!(!exact.contains(&scope("C08",&d,json!({"libraryIdPrefix":""}),Some(&[inside]),"changed-package")));
    }
    #[test]
    fn filtered_families_and_composite_shared_personal_identities_keep_their_native_meaning() {
        let d=vec![filter("schemaTableIdPrefix","c02-schema-table-id-prefix-v1"),PluginSetting::choice("definitionFamily","Family","schemaAndAllMappings",[("Schema","schema"),("Mappings","mappings"),("Both","schemaAndAllMappings")].map(|(label,value)|herbert_contract::PluginSettingOption{label:label.into(),value:value.into()})).description("Matching contract: c02-definition-family-v1.")];
        let mapping=identity("C02","UDMapping",herbert_contract::Scope::Global,&[("MapTableID","17")]);
        let exact=scope("C02",&d,json!({"definitionFamily":"schema","schemaTableIdPrefix":"Ignored"}),Some(&[mapping]),"p");
        assert!(scope("C02",&d,json!({"definitionFamily":"mappings","schemaTableIdPrefix":"No relation to maps"}),None,"p").contains(&exact));
        assert!(!scope("C02",&d,json!({"definitionFamily":"schema","schemaTableIdPrefix":""}),None,"p").contains(&exact));
        let d=vec![filter("layerNamePrefix","c09-layer-name-prefix-v1"),filter("formNamePrefix","c09-form-name-prefix-v1")];
        let global=identity("C09","PersistedXXXDef",herbert_contract::Scope::Global,&[("Company",""),("ProductID","EP"),("TypeCode","Customization"),("Key1","CustomA"),("Key2","Order"),("Key3","Device"),("CGCCode","")]);
        let full=scope("C09",&d,json!({"layerNamePrefix":"Custom","formNamePrefix":"Ord"}),None,"p");
        let exact=scope("C09",&d,json!({"layerNamePrefix":"Ignored","formNamePrefix":"Ignored"}),Some(std::slice::from_ref(&global)),"p");assert!(full.contains(&exact));
        let mut company=global.clone();company.scope=herbert_contract::Scope::Company{company:"C".into()};company.keys.insert("Company".into(),"C".into());
        assert!(full.contains(&scope("C09",&d,json!({"layerNamePrefix":"","formNamePrefix":""}),Some(std::slice::from_ref(&company)),"p")));
        company.scope=herbert_contract::Scope::Company{company:"FOREIGN".into()};company.keys.insert("Company".into(),"FOREIGN".into());assert!(!full.contains(&scope("C09",&d,json!({"layerNamePrefix":"","formNamePrefix":""}),Some(&[company]),"p")));
        let mut other=global.clone();other.keys.insert("Key3".into(),"AnotherDevice".into());assert!(!exact.contains(&scope("C09",&d,json!({"layerNamePrefix":"","formNamePrefix":""}),Some(&[other]),"p")));
        let mut personal=global;personal.scope=herbert_contract::Scope::Personal{company:"C".into(),owner:"user".into()};assert!(!exact.contains(&scope("C09",&d,json!({"layerNamePrefix":"","formNamePrefix":""}),Some(&[personal]),"p")));
        assert!(!scope("C09",&d,json!({"layerNamePrefix":"Custom","formNamePrefix":"Other"}),None,"p").contains(&exact));
    }
    #[test]
    fn c13_combined_read_is_known_tuning_in_both_directions_without_weakening_material_facts() {
        let checkbox=PluginSetting::checkbox("combinedRead","Combine source reads in one BAQ",false).description("Read personalization source groups together with an unsaved BAQ. File dependencies still use their native readers. Your choice is saved.");
        let declarations=vec![checkbox.clone(),filter("userIdPrefix","c13-user-id-prefix-v1")];
        assert!(crate::application::is_tuning_setting("C13",&checkbox));assert!(!crate::application::is_tuning_setting("C12",&checkbox));
        let selected=|combined,prefix:&str,module:&str|{let mut s=scope("C13",&declarations,json!({"combinedRead":combined,"userIdPrefix":prefix}),None,module);s.capabilities.insert("$native-access".into(),Capability{binding:"stored-native-policy-remedy-v1".into(),selection:BTreeMap::new(),effects:BTreeMap::from([("access:Ice.BO.DynamicQuerySvc/Execute/GROUP".into(),json!({"kind":"groupMembership","service":"Ice.BO.DynamicQuerySvc","method":"Execute","group":"GROUP","policy":{"EntryList":"GROUP"}}))])});s};
        let root=tempfile::tempdir().unwrap();
        for baseline in [false,true] {
            let before=selected(baseline,"Ab","module");let after=selected(!baseline,"Ab","module");assert_eq!(before.digest(),after.digest());
            let grant=server_changes::create(root.path(),before.clone(),Lifetime::Session{session_id:"session".into()},None,"explicitLocalClient").unwrap();
            assert!(grant.permits(&after,&server_changes::Use{request_id:"another-logical-run",session_id:"session",workflow_id:None}));
            assert!(!grant.scope.contains(&selected(!baseline,"Other","module")));assert!(!grant.scope.contains(&selected(!baseline,"Ab","changed-module")));
            let mut changed_effect=after.clone();changed_effect.capabilities.get_mut("$native-access").unwrap().effects.values_mut().next().unwrap()["policy"]["EntryList"]=json!("OTHER");assert!(!grant.scope.contains(&changed_effect));
            let owner=identity("C13","PersistedKineticPersonalization",herbert_contract::Scope::Personal{company:"C".into(),owner:"OtherOwner".into()},&[("Company","C"),("ProductID","EP"),("TypeCode","Personalization"),("Key1","App"),("Key2","Layer"),("Key3",""),("CGCCode","")]);
            let mut outside=scope("C13",&declarations,json!({"combinedRead":!baseline,"userIdPrefix":"Ignored"}),Some(&[owner]),"module");outside.capabilities.insert("$native-access".into(),after.capabilities["$native-access"].clone());assert!(!grant.scope.contains(&outside));
        }
        let unknown=PluginSetting::checkbox("combinedRead","Unreviewed meaning",false).description("Different semantics");assert!(!crate::application::is_tuning_setting("C13",&unknown));
        assert!(matches!(&selection("C13",&[unknown],&json!({"combinedRead":true}))["combinedRead"],Selection::Exact{..}));
        assert!(matches!(&selection("C12",&[checkbox],&json!({"combinedRead":true}))["combinedRead"],Selection::Exact{..}));
    }
    #[test]
    fn native_file_effects_follow_actual_families_and_exact_roots() {
        let declarations=vec![PluginSetting::choice("definitionFamily","Family","reportDataAndBaqReports",[("RDD","reportData"),("BAQ","baqReports"),("Both","reportDataAndBaqReports")].map(|(label,value)|herbert_contract::PluginSettingOption{label:label.into(),value:value.into()})).description("Matching contract: c16-definition-family-v1.")];
        let selected=|values:&Value,full,ids:&[Identity]|native_file_effect_selected("C16","Ice.BO.DynamicReportSvc","ExportToFile",&declarations,values,full,ids);
        assert!(!selected(&json!({"definitionFamily":"reportData"}),true,&[]));assert!(selected(&json!({"definitionFamily":"baqReports"}),true,&[]));assert!(selected(&json!({"definitionFamily":"reportDataAndBaqReports"}),true,&[]));
        let rdd=identity("C16","ReportDataDefinition",herbert_contract::Scope::Global,&[("RptDefID","R")]);let baq=identity("C16","BAQReport",herbert_contract::Scope::Global,&[("BAQRptID","B")]);
        assert!(!selected(&json!({"definitionFamily":"baqReports"}),false,&[rdd]));assert!(selected(&json!({"definitionFamily":"reportData"}),false,&[baq]));
    }
    #[test]
    fn c04_full_baq_and_ubaq_grants_cover_only_native_query_subsets() {
        let d=vec![filter("queryIdPrefix","c04-query-id-prefix-v1")];
        let baq=identity("C04","BAQ",herbert_contract::Scope::Company{company:"C".into()},&[("Company","C"),("QueryID","AbOrdinary")]);
        let ubaq=identity("C04","UBAQ",herbert_contract::Scope::Global,&[("Company",""),("QueryID","AbUpdate")]);
        let ids=[baq.clone(),ubaq.clone()];let exact=scope("C04",&d,json!({"queryIdPrefix":"Ignored"}),Some(&ids),"p");
        assert!(scope("C04",&d,json!({"queryIdPrefix":""}),None,"p").contains(&exact));assert!(scope("C04",&d,json!({"queryIdPrefix":"Ab"}),None,"p").contains(&exact));
        assert!(!scope("C04",&d,json!({"queryIdPrefix":"ab"}),None,"p").contains(&exact));assert!(!scope("C04",&d,json!({"queryIdPrefix":"AbOrdinary"}),None,"p").contains(&exact));
        let one=scope("C04",&d,json!({"queryIdPrefix":"Changed but ignored"}),Some(std::slice::from_ref(&ubaq)),"p");assert!(exact.contains(&one));assert!(!one.contains(&exact));
        let mut outside=ubaq;outside.keys.insert("QueryID".into(),"Outside".into());assert!(!scope("C04",&d,json!({"queryIdPrefix":"Ab"}),None,"p").contains(&scope("C04",&d,json!({"queryIdPrefix":"Ab"}),Some(&[outside]),"p")));
        let mut reclassified=baq;reclassified.kind="UBAQ".into();assert!(!exact.contains(&scope("C04",&d,json!({"queryIdPrefix":""}),Some(&[reclassified]),"p")));
    }
    #[test]
    fn shared_company_preserves_physical_owner_and_selected_visibility_context() {
        let d=vec![filter("menuIdPrefix","c15-menu-id-prefix-v1")];
        let shared=identity("C15","PersistedMenu",herbert_contract::Scope::SharedCompany{company:"C".into(),owner_company:"Peer".into()},&[("Company","Peer"),("MenuID","MenuA")]);
        let full=scope("C15",&d,json!({"menuIdPrefix":"Menu"}),None,"p");
        let exact=scope("C15",&d,json!({"menuIdPrefix":"Ignored"}),Some(std::slice::from_ref(&shared)),"p");assert!(full.contains(&exact));
        let mut other=shared.clone();other.scope=herbert_contract::Scope::SharedCompany{company:"ForeignContext".into(),owner_company:"Peer".into()};assert!(!full.contains(&scope("C15",&d,json!({"menuIdPrefix":""}),Some(&[other]),"p")));
        let mut changed_owner=shared;changed_owner.keys.insert("Company".into(),"UnrelatedOwner".into());assert!(!full.contains(&scope("C15",&d,json!({"menuIdPrefix":""}),Some(&[changed_owner]),"p")));
    }
    #[test]
    fn c12_page_controls_are_tuning_but_history_prefix_and_packages_are_material() {
        let d=vec![PluginSetting::integer("itemsPerPage","Batch",100,1,i32::MAX as u32),PluginSetting::integer("metadataRowsPerPage","Metadata",1000,1,i32::MAX as u32),filter("applicationIdPrefix","c12-application-id-prefix-v1"),PluginSetting::checkbox("includeVersionHistory","History",true).description("Collect older saved revisions. Turn this off for current published definitions and saved drafts only. Your choice is saved with this plugin.")];
        let a=scope("C12",&d,json!({"itemsPerPage":10,"metadataRowsPerPage":100,"applicationIdPrefix":"App","includeVersionHistory":true}),None,"p");
        let b=scope("C12",&d,json!({"itemsPerPage":30,"metadataRowsPerPage":3000,"applicationIdPrefix":"App","includeVersionHistory":true}),None,"p");assert_eq!(a.digest(),b.digest());assert!(a.contains(&b));
        let current=scope("C12",&d,json!({"applicationIdPrefix":"App","includeVersionHistory":false}),None,"p");assert_ne!(a.digest(),current.digest());assert!(a.contains(&current));assert!(!current.contains(&a));
        assert!(!a.contains(&scope("C12",&d,json!({"applicationIdPrefix":"Other","includeVersionHistory":true}),None,"p")));
        assert!(!a.contains(&scope("C12",&d,json!({"applicationIdPrefix":"App","includeVersionHistory":true}),None,"changed")));
        let id=identity("C12","KineticApplicationBundle",herbert_contract::Scope::Company{company:"C".into()},&[("Company","C"),("ViewId","App.One")]);
        assert!(a.contains(&scope("C12",&d,json!({"applicationIdPrefix":"Ignored","includeVersionHistory":false}),Some(&[id]),"p")));
        assert!(!needed("compiled_functions",&json!({"downloadCompiledAssemblies":false,"decompileCompiledAssemblies":false})));assert!(needed("compiled_functions",&json!({"decompileCompiledAssemblies":true})));assert!(!needed("history",&json!({"includeVersionHistory":false})));
    }
}
