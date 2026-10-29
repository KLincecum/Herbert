//! The GUI consumes the same host-issued preview and explicit grant operations.
use super::*;
#[derive(Clone)]
pub(super) struct PendingDownload { pub request:Value, pub preview:Value, pub connection_key:String }
#[derive(Clone,Copy,Default,PartialEq,Eq)]
pub(super) enum AuthorizationChoice { #[default] OneRun, Session, Workflow }

pub(super) fn connection_key(arguments:&Value,transport:bool)->String {
    let mut selected=arguments.clone();
    let settings=selected.as_object_mut().and_then(|value|value.remove("settings")).unwrap_or(Value::Null);
    if transport {
        // These are the fields Connection retains for HTTP behavior. Collection
        // concurrency, page sizes and local workers do not force a reconnect.
        let fixed=["connectionTimeoutMs","serviceTimeoutMs","exportTimeoutMs","inactivityTimeoutMs","retryAttempts","retryBackoffMs"].into_iter().map(|key|(key.to_owned(),settings[key].clone())).collect::<serde_json::Map<_,_>>();
        selected["connectionSettings"]=Value::Object(fixed);
    }
    if selected.pointer("/credentialSource/type").and_then(Value::as_str)==Some("userFile") {
        if let Some(path)=selected.pointer("/credentialSource/path").and_then(Value::as_str) {
            let observed=std::fs::metadata(path).ok().map(|metadata|json!({"bytes":metadata.len(),"modified":metadata.modified().ok().and_then(|time|time.duration_since(std::time::UNIX_EPOCH).ok()).map(|time|time.as_nanos().to_string())}));
            selected["userFileMetadata"]=json!(observed);
        }
    }
    crate::storage::sha256(&serde_json::to_vec(&selected).expect("connection arguments JSON"))
}
pub(super) fn authorization_label(grant:&Value)->String {
    let lifetime=match grant.pointer("/lifetime/kind").and_then(Value::as_str) {
        Some("oneRun")=>"this logical run".to_owned(),Some("session")=>"this connection session".to_owned(),
        Some("workflow")=>format!("named workflow {}",grant.pointer("/lifetime/workflowId").and_then(Value::as_str).unwrap_or("unknown")),
        _=>"inspect the original grant".to_owned(),
    };
    format!("Authorization {} · {lifetime}",grant["id"].as_str().unwrap_or("unknown"))
}
fn automatically_applicable(preview:&Value)->Option<Option<String>> {
    if !preview["unresolved"].as_array().is_some_and(Vec::is_empty) { return None; }
    if preview["authorizationRequired"]==false { return Some(None); }
    preview["matchingAuthorizations"].as_array()?.iter().find_map(|grant|grant["id"].as_str().map(|id|Some(id.to_owned())))
}
/// Returns true when Saved authorizations… is clicked.
pub(super) fn lifetime_controls(ui:&mut egui::Ui,choice:&mut AuthorizationChoice,workflow:&mut String)->bool {
    ui.label("Authorization lifetime when a new grant is needed");
    let manage=ui.horizontal_wrapped(|ui| {
        ui.radio_value(choice,AuthorizationChoice::OneRun,"One logical run");
        ui.radio_value(choice,AuthorizationChoice::Session,"This connection session");
        ui.radio_value(choice,AuthorizationChoice::Workflow,"Named workflow");
        ui.button("Saved authorizations…").on_hover_text("Review or revoke saved session and named workflow grants.").clicked()
    }).inner;
    if *choice==AuthorizationChoice::Workflow {
        let label=ui.label("Workflow ID");ui.text_edit_singleline(workflow).labelled_by(label.id);
        ui.label("A named grant does not create or activate a schedule.");
    }
    manage
}

/// Saved session and named workflow grants, read and revoked through the host's
/// serverChanges.list and serverChanges.revoke operations.
#[derive(Default)]
pub(super) struct SavedAuthorizations {
    open: bool,
    /// None until the first list reply after opening.
    grants: Option<Vec<Value>>,
    loading: bool,
    revoking: Option<String>,
    confirm: Option<String>,
    notice: String,
    error: Option<Diagnostic>,
    replies: Arc<std::sync::Mutex<Vec<(&'static str, Reply)>>>,
}
enum SavedAction { Refresh, Confirm(String), Keep, Revoke(String) }
impl SavedAuthorizations {
    fn request(&mut self,host:&Arc<Host>,runtime:&tokio::runtime::Handle,ctx:&egui::Context,method:&'static str,arguments:Value) {
        let host=host.clone();let replies=self.replies.clone();let ctx=ctx.clone();
        runtime.spawn(async move {
            let reply=host.execute(Command{schema_version:1,method:method.into(),arguments}).await;
            replies.lock().unwrap_or_else(|poisoned|poisoned.into_inner()).push((method,reply));
            ctx.request_repaint();
        });
    }
    /// Applies finished replies. Returns true when the list must be read again.
    fn receive(&mut self)->bool {
        let replies=std::mem::take(&mut *self.replies.lock().unwrap_or_else(|poisoned|poisoned.into_inner()));
        let mut reload=false;
        for (method,reply) in replies {
            match (method,reply.error) {
                ("serverChanges.list",None)=>{self.loading=false;self.grants=Some(reply.result.and_then(|value|value["authorizations"].as_array().cloned()).unwrap_or_default());},
                ("serverChanges.list",Some(error))=>{self.loading=false;self.error=Some(error);},
                ("serverChanges.revoke",None)=>{self.revoking=None;self.notice="Authorization revoked. New optional server changes under it stop. Proven owned cleanup still runs.".into();reload=true;},
                ("serverChanges.revoke",Some(error))=>{self.revoking=None;self.error=Some(error);reload=true;},
                _=>{},
            }
        }
        reload
    }
}
/// Grants that can still be revoked: session and named workflow lifetimes, newest first.
fn revocable(grants:&[Value])->Vec<&Value> {
    let mut rows=grants.iter().filter(|grant|matches!(grant.pointer("/lifetime/kind").and_then(Value::as_str),Some("session"|"workflow"))&&grant["revokedAt"].is_null()).collect::<Vec<_>>();
    rows.sort_by_key(|grant|std::cmp::Reverse(grant["createdAt"].as_u64().unwrap_or(0)));
    rows
}
fn saved_lifetime(grant:&Value)->String {
    match grant.pointer("/lifetime/kind").and_then(Value::as_str) {
        Some("workflow")=>format!("Named workflow {}",grant.pointer("/lifetime/workflowId").and_then(Value::as_str).unwrap_or("unknown")),
        Some("session")=>"Connection session".to_owned(),
        _=>"Other lifetime".to_owned(),
    }
}
/// What the grant allows, by plugin, then the account it is bound to.
fn grant_allows(grant:&Value,names:&BTreeMap<String,String>)->String {
    let scope=&grant["scope"];
    let mut parts=Vec::new();
    for (id,capability) in scope["capabilities"].as_object().into_iter().flatten() {
        let kinds=capability["effects"].as_object().into_iter().flatten().map(|(_,effect)|match effect["kind"].as_str().unwrap_or("") {
            "helper"|"hostHelper"|"legacyHelper"=>"temporary helpers",
            "groupMembership"|"borrowOwnedMembership"=>"temporary security group membership",
            "nativeExportFiles"=>"temporary server export files",
            _=>"other server changes",
        }).collect::<std::collections::BTreeSet<_>>();
        if kinds.is_empty() {continue;}
        parts.push(format!("{} for {}",kinds.into_iter().collect::<Vec<_>>().join(" and "),names.get(id).map_or(id.as_str(),String::as_str)));
    }
    let allows=if parts.is_empty() {"No server changes".to_owned()} else {
        let mut text=parts.join("; ");text[..1].make_ascii_uppercase();text
    };
    format!("{allows}. Account {} in company {} at {}.",scope["principal"].as_str().unwrap_or("unknown"),scope["company"].as_str().unwrap_or("unknown"),scope["endpoint"].as_str().unwrap_or("unknown"))
}
fn saved_expired(grant:&Value)->bool {
    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    grant["expiresAt"].as_u64().is_some_and(|expiry|expiry<=now)
}
/// Seconds since 1970 as a UTC date and minute, from Howard Hinnant's civil-from-days.
fn utc_time(seconds:u64)->String {
    let days=(seconds/86_400) as i64;let rest=seconds%86_400;
    let z=days+719_468;let era=z.div_euclid(146_097);let doe=z-era*146_097;
    let yoe=(doe-doe/1_460+doe/36_524-doe/146_096)/365;
    let doy=doe-(365*yoe+yoe/4-yoe/100);let mp=(5*doy+2)/153;
    let day=doy-(153*mp+2)/5+1;let month=if mp<10 {mp+3} else {mp-9};
    let year=yoe+era*400+i64::from(month<=2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",rest/3_600,rest%3_600/60)
}
impl Gui {
    pub(super) async fn preview_or_start(host:&Arc<Host>,sender:&mpsc::Sender<Event>,mut request:Value,key:String)->Option<Reply> {
        let reply=host.execute(Command{schema_version:1,method:"serverChanges.preview".into(),arguments:json!({"fullBackup":true,"run":request})}).await;
        if !reply.ok {return Some(reply);}
        let preview=reply.result.unwrap_or(Value::Null);
        if let Some(grant)=automatically_applicable(&preview) {
            if let Some(id)=grant {
                if let Some(selected)=preview["matchingAuthorizations"].as_array().and_then(|grants|grants.iter().find(|grant|grant["id"].as_str()==Some(id.as_str()))) {let _=sender.send(Event::UsingAuthorization(json!({"id":selected["id"],"lifetime":selected["lifetime"]})));}
                request["authorizationId"]=json!(id);
            }
            return Some(host.execute(Command{schema_version:1,method:"backup.start".into(),arguments:request}).await);
        }
        let _=sender.send(Event::DownloadPreview(PendingDownload{request,preview,connection_key:key}));
        None
    }
    pub(super) fn server_change_window(&mut self,ctx:&egui::Context) {
        let Some(pending)=self.pending_download.clone() else {return;};
        let mut confirm=false;let mut cancel=false;let mut direct=false;let mut manage=false;
        let unresolved=pending.preview["unresolved"].as_array().is_none_or(|items|!items.is_empty());
        egui::Window::new("Review temporary server changes").resizable(true).default_width(720.0).max_height(ctx.content_rect().height()-40.0).show(ctx,|ui| {
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-360.0).max(120.0)).show(ui,|ui| {
            let display=&pending.preview["display"];
            ui.heading("Selected account and collection");
            ui.label(format!("{} / {} / {}",display["endpoint"].as_str().unwrap_or("Unknown endpoint"),display["company"].as_str().unwrap_or("Unknown company"),display["principal"].as_str().unwrap_or("Unknown caller")));
            ui.label("Account readiness: verified eligible for this admitted connection. Eligibility does not authorize server changes.");
            ui.label(format!("Method: {}. Temporary access: {}.",display["collectionMethod"].as_str().unwrap_or("Unknown"),if display["temporaryAccess"]==true{"on"}else{"off"}));
            ui.label(format!("Destination: {}",pending.request["destination"].as_str().unwrap_or("Unknown")));
            if let Some(categories)=display["capabilities"].as_array() {for category in categories {
                ui.separator();ui.label(RichText::new(format!("{}: {}",category["category"].as_str().unwrap_or(""),category["name"].as_str().unwrap_or(""))).strong());
                ui.label(format!("Selected content: {}",category["effectiveSettings"]));
            }}
            ui.separator();ui.heading("Possible server effects");
            if let Some(capabilities)=pending.preview["materialScope"]["capabilities"].as_object() {for (category,capability) in capabilities {if let Some(effects)=capability["effects"].as_object() {for (key,effect) in effects {
                ui.label(RichText::new(format!("{category}: {key}")).strong());
                match effect["kind"].as_str().unwrap_or("") {
                    "helper"|"hostHelper" => {
                        let declaration=&effect["declaration"];
                        ui.label(format!("Native component: {}. Lifetime: {}. Target: {}.",declaration["type"],declaration["lifetime"],declaration["target"]));
                        if let Some(description)=declaration.pointer("/effects/description").and_then(Value::as_str){ui.label(description);}
                        ui.label(format!("Package: {}. SHA256: {}",effect["packageFile"].as_str().unwrap_or("Unknown"),effect["packageSha256"].as_str().unwrap_or("Unknown")));
                    },
                    "legacyHelper" => {ui.label("Declared Function helper. A private instance is allocated only when needed.");ui.label(format!("Native target version: {}. Package SHA256: {}",effect["nativeVersion"],effect["packageSha256"]));},
                    "groupMembership"|"borrowOwnedMembership" => {ui.label(format!("Group {} for {} / {}. Existing ownership intent: {}.",effect["group"],effect["service"],effect["method"],effect["ownershipIntent"]));},
                    "nativeExportFiles" => {ui.label(format!("Native exporter {} / {} can create temporary files in server folder {}.",effect["service"],effect["method"],effect["folder"]));},
                    _ => {ui.label(effect.to_string());},
                }
                if let Some(condition)=effect["condition"].as_str(){ui.label(condition);}
                if let Some(cleanup)=effect["cleanup"].as_str(){ui.label(format!("Cleanup: {cleanup}"));}
            }}}}
            ui.label("Herbert allocates exact private identities only when needed. Run results record actual objects and cleanup evidence. Direct/helper permission and temporary-access permission remain independent.");
            if unresolved {
                ui.colored_label(ui.visuals().error_fg_color,"This preview has unresolved facts. It cannot create a grant.");
                if let Some(errors)=pending.preview["unresolved"].as_array(){for error in errors {ui.label(format!("{}: {}",error["code"].as_str().unwrap_or("Unresolved"),error["message"].as_str().unwrap_or("Inspect the diagnostic.")));}}
            }
            });
            manage=lifetime_controls(ui,&mut self.authorization_choice,&mut self.authorization_workflow);
            ui.label("Revocation stops new optional changes. Proven owned cleanup remains attributed to the original operation. Cleanup can remain pending or unknown and must be inspected separately from collection completeness.");
            let valid=!unresolved&&(self.authorization_choice!=AuthorizationChoice::Workflow||!self.authorization_workflow.trim().is_empty());
            ui.horizontal_wrapped(|ui| {
                let authorize=super::gui_connection::primary_button(ui,"Authorize these changes and start",egui::Vec2::ZERO);
                confirm=ui.add_enabled(valid&&!self.working,authorize).clicked();
                direct=ui.add_enabled(!self.working,egui::Button::new("Preview Direct without temporary access")).clicked();
                cancel=ui.add_enabled(!self.working,egui::Button::new("Cancel preview")).clicked();
            });
            ui.label("The Direct alternative can omit helper-dependent content. Its new preview still shows any native exporter file effects.");
        });
        if manage {self.open_saved_authorizations();}
        if confirm {self.authorize_pending_download(ctx);}
        else if direct {self.preview_direct_without_access(ctx);}
        else if cancel {self.pending_download=None;self.notice="Preview canceled. No authorization or collection started.".into();}
    }
    /// Opens the window with a fresh list, so grants saved since the last look appear.
    pub(super) fn open_saved_authorizations(&mut self) {
        let saved=&mut self.saved_authorizations;
        saved.open=true;saved.confirm=None;saved.notice.clear();saved.error=None;
        if !saved.loading {saved.grants=None;}
    }
    fn load_saved_authorizations(&mut self,ctx:&egui::Context) {
        self.saved_authorizations.loading=true;
        self.saved_authorizations.request(&self.host,&self.runtime,ctx,"serverChanges.list",json!({}));
    }
    fn revoke_saved_authorization(&mut self,ctx:&egui::Context,id:String) {
        let saved=&mut self.saved_authorizations;
        saved.confirm=None;saved.notice.clear();saved.error=None;saved.revoking=Some(id.clone());
        saved.request(&self.host,&self.runtime,ctx,"serverChanges.revoke",json!({"id":id}));
    }
    pub(super) fn saved_authorizations_window(&mut self,ctx:&egui::Context) {
        if self.saved_authorizations.receive() {self.load_saved_authorizations(ctx);}
        let saved=&self.saved_authorizations;
        if !saved.open {return;}
        if saved.grants.is_none()&&saved.error.is_none()&&!saved.loading {self.load_saved_authorizations(ctx);}
        let names=self.capabilities.iter().filter_map(|category|Some((category["id"].as_str()?.to_owned(),category["name"].as_str()?.to_owned()))).collect::<BTreeMap<_,_>>();
        let mut open=true;let mut action=None;
        let saved=&self.saved_authorizations;
        egui::Window::new("Saved authorizations").open(&mut open).resizable(true).default_width(640.0).max_height(ctx.content_rect().height()-40.0).show(ctx,|ui| {
            ui.label("Session and named workflow grants let Herbert make the same server changes again without asking. Revoke one to be asked again.");
            ui.horizontal(|ui| {
                if ui.add_enabled(!saved.loading&&saved.revoking.is_none(),egui::Button::new("Refresh")).clicked() {action=Some(SavedAction::Refresh);}
                if saved.loading {ui.spinner();ui.label("Loading saved authorizations…");}
            });
            if !saved.notice.is_empty() {ui.label(&saved.notice);}
            if let Some(error)=&saved.error {
                ui.label(RichText::new(&error.message).color(ui.visuals().error_fg_color));
                ui.label(RichText::new(&error.code).small());
                super::gui_form::technical(ui,"saved-authorizations-technical",technical_details(error).as_deref());
            }
            let Some(grants)=&saved.grants else {return;};
            let rows=revocable(grants);
            if rows.is_empty() {
                ui.label(RichText::new("No saved authorizations. Session and named workflow grants appear here after you authorize server changes.").weak());
                return;
            }
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-220.0).max(120.0)).show(ui,|ui| {
                for grant in rows {
                    let id=grant["id"].as_str().unwrap_or("");
                    egui::Frame::group(ui.style()).inner_margin(8).corner_radius(6).show(ui,|ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(saved_lifetime(grant)).strong());
                            if saved_expired(grant) {ui.label(RichText::new("Expired").small().weak());}
                        });
                        ui.label(grant_allows(grant,&names));
                        ui.label(RichText::new(format!("Created {} · Authorization {id}",grant["createdAt"].as_u64().map_or_else(||"at an unknown time".to_owned(),utc_time))).small().weak());
                        if saved.confirm.as_deref()==Some(id) {
                            ui.label("Revoke this authorization? New optional server changes under it stop, including in a download that is running. Proven owned cleanup still runs.");
                            ui.horizontal(|ui| {
                                if ui.button("Revoke authorization").clicked() {action=Some(SavedAction::Revoke(id.to_owned()));}
                                if ui.button("Keep it").clicked() {action=Some(SavedAction::Keep);}
                            });
                        } else if saved.revoking.as_deref()==Some(id) {
                            ui.horizontal(|ui| {ui.spinner();ui.label("Revoking…");});
                        } else if ui.add_enabled(saved.revoking.is_none(),egui::Button::new("Revoke…")).clicked() {
                            action=Some(SavedAction::Confirm(id.to_owned()));
                        }
                    });
                }
            });
        });
        self.saved_authorizations.open&=open;
        match action {
            Some(SavedAction::Refresh)=>{self.saved_authorizations.notice.clear();self.saved_authorizations.error=None;self.load_saved_authorizations(ctx);},
            Some(SavedAction::Confirm(id))=>{self.saved_authorizations.confirm=Some(id);},
            Some(SavedAction::Keep)=>{self.saved_authorizations.confirm=None;},
            Some(SavedAction::Revoke(id))=>self.revoke_saved_authorization(ctx,id),
            None=>{},
        }
    }
    fn preview_direct_without_access(&mut self,ctx:&egui::Context) {
        self.pending_download=None;self.collection_method=herbert_contract::CollectionMethod::DirectOnly;self.temporary_access=false;self.start_download(ctx);
    }
    fn authorize_pending_download(&mut self,ctx:&egui::Context) {
        let Some(mut pending)=self.pending_download.take() else {return;};
        let current=self.settings.value().and_then(|settings|self.connection_arguments(&settings)).map(|value|connection_key(&value,true));
        if !self.connected||self.cleanup_pending||current.ok().as_deref()!=Some(pending.connection_key.as_str())||self.connected_key.as_deref()!=Some(pending.connection_key.as_str()) {
            self.show_error(Diagnostic::new("SERVER_CHANGE_PREVIEW_REQUIRED","Connection inputs changed. Obtain a new preview before authorization."));return;
        }
        let lifetime=match self.authorization_choice {
            AuthorizationChoice::OneRun=>json!({"kind":"oneRun"}),
            AuthorizationChoice::Session=>json!({"kind":"session"}),
            AuthorizationChoice::Workflow=>{pending.request["workflowId"]=json!(self.authorization_workflow);json!({"kind":"workflow","workflowId":self.authorization_workflow})},
        };
        self.working=true;self.notice="Authorizing the reviewed effects…".into();
        let host=self.host.clone();let sender=self.sender.clone();let ctx=ctx.clone();
        self.runtime.spawn(async move {
            let reply=match host.authorize_interactive_server_changes(json!({"previewId":pending.preview["previewId"],"materialDigest":pending.preview["materialDigest"],"lifetime":lifetime})).await {
                Ok(result)=>match result.pointer("/authorization/id").and_then(Value::as_str) {
                    Some(id)=>{let _=sender.send(Event::UsingAuthorization(json!({"id":result["authorization"]["id"],"lifetime":result["authorization"]["lifetime"]})));pending.request["authorizationId"]=json!(id);host.execute(Command{schema_version:1,method:"backup.start".into(),arguments:pending.request}).await},
                    None=>Reply{schema_version:1,ok:false,result:None,error:Some(Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED","The explicit authorization result has no grant identity. Inspect it before starting."))},
                },
                Err(error)=>Reply{schema_version:1,ok:false,result:None,error:Some(error)},
            };
            let _=sender.send(Event::Started(reply));ctx.request_repaint();
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_resolved_no_effect_or_matching_host_authority_starts_automatically() {
        assert_eq!(automatically_applicable(&json!({"unresolved":[],"authorizationRequired":false})),Some(None));
        assert_eq!(automatically_applicable(&json!({"unresolved":[],"authorizationRequired":true,"matchingAuthorizations":[{"id":"existing"}]})),Some(Some("existing".into())));
        assert_eq!(automatically_applicable(&json!({"unresolved":[],"authorizationRequired":true,"matchingAuthorizations":[]})),None);
        assert_eq!(automatically_applicable(&json!({"unresolved":[{"code":"UNKNOWN"}],"authorizationRequired":false})),None);
        assert_eq!(automatically_applicable(&Value::Null),None);
    }
    #[test]
    fn session_identity_preserves_collection_tuning_and_binds_fixed_connection_settings() {
        let base=json!({"url":"https://example.invalid/Epicor","company":"C","credentialSource":{"type":"inline","credentials":{"username":"u","password":"fixture"}},"settings":{"concurrency":4,"localDecompilerWorkers":4,"pageSize":500,"serviceTimeoutMs":null,"retryAttempts":0}});
        let mut tuning=base.clone();tuning["settings"]["concurrency"]=json!(7);tuning["settings"]["pageSize"]=json!(900);assert_eq!(connection_key(&base,true),connection_key(&tuning,true));
        let mut transport=base.clone();transport["settings"]["serviceTimeoutMs"]=json!(120000);assert_ne!(connection_key(&base,true),connection_key(&transport,true));assert_eq!(connection_key(&base,false),connection_key(&transport,false));
        let mut target=base.clone();target["company"]=json!("Other");assert_ne!(connection_key(&base,true),connection_key(&target,true));assert_ne!(connection_key(&base,false),connection_key(&target,false));
        let mut principal=base.clone();principal["credentialSource"]["credentials"]["username"]=json!("peer");assert_ne!(connection_key(&base,false),connection_key(&principal,false));
    }
    fn visible_preview(unresolved:bool)->String {visible_preview_for(unresolved,false)}
    fn visible_preview_for(unresolved:bool,helper:bool)->String {
        let ctx=egui::Context::default();let runtime=tokio::runtime::Runtime::new().unwrap();
        let mut app=super::super::gui_progress_layout_tests::app(&ctx,&runtime,false);
        app.pending_download=Some(PendingDownload{request:json!({"destination":"C:/backup fixture"}),connection_key:"fixture".into(),preview:json!({"unresolved":if unresolved{json!([{"code":"UNKNOWN_EFFECT","message":"Inspect the native policy before authorizing."}])}else{json!([])},"display":{"endpoint":"https://example.invalid/Epicor","company":"C","principal":"operator","collectionMethod":"directOnly","temporaryAccess":true,"capabilities":[]},"materialScope":{"capabilities":{"report-definitions":{"effects":{"nativeFile:export":{"kind":"nativeExportFiles","service":"NativeReportSvc","method":"ExportToFile","folder":4,"cleanup":"Verify native deletion"}}}}}})});
        if helper {app.pending_download.as_mut().unwrap().preview["materialScope"]["capabilities"]["report-definitions"]["effects"]=json!({"helper:reader":{"kind":"helper","declaration":{"type":"functionLibrary","lifetime":"temporary","target":null,"effects":{"description":"Read the selected compiled definitions"}},"packageFile":"reader.efxj","packageSha256":"fixture-sha"}});}
        let mut labels=String::new();
        for _ in 0..3 {
            let mut output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1180.0,900.0))),..Default::default()},|ui|{app.server_change_window(ui.ctx());});
            output.textures_delta.clear();labels=output.shapes.into_iter().filter_map(|shape|if let egui::Shape::Text(text)=shape.shape {let rect=egui::Rect::from_min_size(text.pos,text.galley.size());shape.clip_rect.expand(1.0).contains_rect(rect).then(||text.galley.text().to_owned())}else{None}).collect::<Vec<_>>().join(" ");
        }labels
    }
    #[test]
    fn preview_visibly_separates_eligibility_effects_lifetime_and_confirmation() {
        let text=visible_preview(false);
        for expected in ["Review temporary server changes","verified eligible","does not authorize server changes","directOnly","NativeReportSvc","ExportToFile","One logical run","This connection session","Named workflow","Authorize these changes and start","Preview Direct without temporary access","Cancel preview"] {assert!(text.contains(expected),"Missing visible text: {expected}. Actual: {text}");}
    }
    #[test]
    fn unresolved_preview_keeps_the_diagnostic_and_safe_alternative_visible() {
        let text=visible_preview(true);assert!(text.contains("UNKNOWN_EFFECT"));assert!(text.contains("cannot create a grant"));assert!(text.contains("Preview Direct without temporary access"));
    }

    async fn settle_action(app:&mut Gui,ctx:&egui::Context) {
        // A local loopback action normally settles within the existing API
        // fixture's measured second. This guard diagnoses a hung test only.
        tokio::time::timeout(std::time::Duration::from_secs(30),async {
            while app.working {app.receive(ctx);tokio::task::yield_now().await;}
            app.receive(ctx);
        }).await.expect("GUI loopback action did not settle");
    }
    async fn finish_test_run(app:&mut Gui,ctx:&egui::Context)->Value {
        assert!(!app.run_id.is_empty(),"{} {:?}",app.notice,app.diagnostic);
        let id=app.run_id.clone();
        let journal=tokio::time::timeout(std::time::Duration::from_secs(30),async {
            loop {
                let reply=app.host.execute(Command{schema_version:1,method:"run.status".into(),arguments:json!({"id":id})}).await;
                assert!(reply.ok,"{:?}",reply.error);let value=reply.result.unwrap();if value["executionActive"]==false {break value;}tokio::task::yield_now().await;
            }
        }).await.expect("Accepted loopback run did not settle");
        app.receive(ctx);
        // Same reset as the existing New download control.
        app.run_id.clear();app.journal=None;app.output_root.clear();app.notice.clear();
        journal
    }
    #[test]
    fn asynchronous_gui_preview_grant_start_session_workflow_stale_and_direct_routes() {
        use crate::application::server_change_api_tests::{Server,module};
        let runtime=tokio::runtime::Runtime::new().unwrap();let ctx=egui::Context::default();let root=tempfile::tempdir().unwrap();let server=Server::new();
        let mut app=super::super::gui_progress_layout_tests::app(&ctx,&runtime,false);
        app.host=Host::new(root.path().join("state"),root.path().join("plugins"));module(&app.host.plugin_root,"Original");
        app.profiles_loaded=true;app.settings_loaded=true;app.capabilities_loaded=true;app.run_id.clear();app.journal=None;
        app.source=Source::Entered;app.url=server.endpoint.to_string();app.company="C".into();app.mode=AuthMode::BasicApiKey;app.username="fixture".into();app.password="fictional".into();app.api_key="fictional".into();app.destination=root.path().join("output").to_string_lossy().into_owned();app.output_mode=Some(false);app.categories=BTreeMap::from([("report-definitions".into(),true)]);app.plugin_settings=BTreeMap::from([("report-definitions".into(),json!({"itemsPerPage":100}))]);app.collection_method=herbert_contract::CollectionMethod::DirectOnly;app.temporary_access=false;
        runtime.block_on(async {
            app.start_download(&ctx);settle_action(&mut app,&ctx).await;
            assert!(app.pending_download.is_some());assert!(crate::server_changes::list(&app.host.state_root).unwrap().is_empty());assert!(!root.path().join("output").exists());
            let first_key=app.connected_key.clone();app.authorization_choice=AuthorizationChoice::Session;app.authorize_pending_download(&ctx);settle_action(&mut app,&ctx).await;
            assert_eq!(app.selected_authorization.as_ref().unwrap()["lifetime"]["kind"],"session");let first=finish_test_run(&mut app,&ctx).await;let session_id=first["serverChangeAuthority"]["grantId"].as_str().unwrap().to_owned();
            let grants=crate::server_changes::list(&app.host.state_root).unwrap();assert_eq!(grants.len(),1);assert_eq!(grants[0].origin,"interactiveGui");
            let before=server.calls.lock().unwrap().iter().filter(|method|method.as_str()=="GetUserFile").count();
            app.plugin_settings.get_mut("report-definitions").unwrap()["itemsPerPage"]=json!(700);app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_none());
            let reused=finish_test_run(&mut app,&ctx).await;assert_eq!(reused["serverChangeAuthority"]["grantId"],session_id);assert!(authorization_label(app.selected_authorization.as_ref().unwrap()).contains(&session_id));assert_eq!(app.connected_key,first_key);assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),1);assert_eq!(server.calls.lock().unwrap().iter().filter(|method|method.as_str()=="GetUserFile").count(),before);
            app.company="Other".into();app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_some());assert_ne!(app.connected_key,first_key);assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),1);app.pending_download=None;
            app.company="C".into();app.authorization_choice=AuthorizationChoice::Workflow;app.authorization_workflow="owned-nightly-fixture".into();app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_some());app.authorize_pending_download(&ctx);settle_action(&mut app,&ctx).await;
            let workflow=finish_test_run(&mut app,&ctx).await;let workflow_id=workflow["serverChangeAuthority"]["grantId"].as_str().unwrap().to_owned();assert_ne!(session_id,workflow_id);
            app.connected_key=None;app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_none());let after_reconnect=finish_test_run(&mut app,&ctx).await;assert_eq!(after_reconnect["serverChangeAuthority"]["grantId"],workflow_id);assert_eq!(after_reconnect["serverChangeAuthority"]["workflowId"],"owned-nightly-fixture");assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),2);
            module(&app.host.plugin_root,"First changed package");app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_some());module(&app.host.plugin_root,"Second changed package");app.authorize_pending_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.run_id.is_empty());assert_eq!(app.diagnostic.as_deref(),Some("SERVER_CHANGE_PREVIEW_REQUIRED"));assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),2);
            app.temporary_access=true;app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(!app.pending_download.as_ref().unwrap().preview["unresolved"].as_array().unwrap().is_empty());app.authorize_pending_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.run_id.is_empty());assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),2);
            let file=root.path().join("selected environments.json");let payload=json!({"schemaVersion":1,"environments":[{"id":"selected","name":"First","url":server.endpoint,"authMode":"basicApiKey","credentials":{"username":"fixture","password":"fictional","apiKey":"fictional"},"defaults":{"company":"C"}}]});std::fs::write(&file,serde_json::to_vec(&payload).unwrap()).unwrap();
            app.source=Source::File;app.temporary_access=false;app.input_file=file.to_string_lossy().into_owned();app.file_choices_path=app.input_file.clone();app.file_environment="selected".into();app.file_choices=vec![Profile{id:"selected".into(),name:Some("First".into()),url:server.endpoint.to_string(),auth_mode:AuthMode::BasicApiKey,defaults:Defaults{company:Some("C".into()),concurrency:None},user_file:None,allow_untrusted_certificate:false}];
            app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_some());let file_key=app.connected_key.clone();app.pending_download=None;let mut changed=payload;changed["environments"][0]["name"]=json!("Metadata changed with a longer name");std::fs::write(&file,serde_json::to_vec(&changed).unwrap()).unwrap();
            app.start_download(&ctx);settle_action(&mut app,&ctx).await;assert!(app.pending_download.is_some());assert_ne!(app.connected_key,file_key);
            app.pending_download=None;app.collection_method=herbert_contract::CollectionMethod::ServerAssisted;app.temporary_access=true;app.start_download(&ctx);settle_action(&mut app,&ctx).await;let old_request=app.pending_download.as_ref().unwrap().request["requestId"].clone();app.preview_direct_without_access(&ctx);settle_action(&mut app,&ctx).await;
            let pending=app.pending_download.as_ref().unwrap();assert_ne!(pending.request["requestId"],old_request);assert_eq!(pending.request["collectionMethod"],"directOnly");assert_eq!(pending.request["temporaryAccess"],false);assert!(pending.preview["unresolved"].as_array().unwrap().is_empty());assert_eq!(pending.preview["authorizationRequired"],true);assert!(pending.preview.pointer("/materialScope/capabilities/report-definitions/effects/nativeFile:Ice.BO.DynamicReportSvc~1ExportToFile").is_some());assert_eq!(crate::server_changes::list(&app.host.state_root).unwrap().len(),2);
            app.pending_download=None;assert!(app.host.shutdown().await.is_empty());
            assert!(server.calls.lock().unwrap().iter().all(|method|matches!(method.as_str(),"GetUserFile"|"GetDatabaseVersion"|"GetFieldList"|"GetByID")),"The loopback GUI fixture attempted an unexpected native action");
        });
    }

    #[test]
    fn helper_native_type_and_package_are_visible_in_the_review() {let text=visible_preview_for(false,true);for expected in ["functionLibrary","temporary","Read the selected compiled definitions","reader.efxj","fixture-sha"]{assert!(text.contains(expected),"Missing helper fact: {expected}. Actual: {text}");}}

    fn saved_grant(id:&str,lifetime:Value,created:u64,revoked:Option<u64>)->Value {
        json!({"id":id,"lifetime":lifetime,"createdAt":created,"expiresAt":null,"revokedAt":revoked,"origin":"interactiveGui","digest":"fixture",
            "scope":{"endpoint":"https://example.invalid/Epicor/","company":"C","principal":"operator","capabilities":{
                "bpm-directives":{"binding":"fixture","selection":{},"effects":{"helper:reader":{"kind":"helper"},"group:x":{"kind":"groupMembership"}}},
                "report-definitions":{"binding":"fixture","selection":{},"effects":{"nativeFile:export":{"kind":"nativeExportFiles"}}},
                "baqs":{"binding":"fixture","selection":{},"effects":{}}}}})
    }
    #[test]
    fn saved_authorizations_keep_live_session_and_workflow_grants_newest_first() {
        let grants=vec![
            saved_grant("run",json!({"kind":"oneRun","requestId":"r"}),10,None),
            saved_grant("session",json!({"kind":"session","sessionId":"s"}),20,None),
            saved_grant("revoked",json!({"kind":"workflow","workflowId":"old"}),40,Some(41)),
            saved_grant("workflow",json!({"kind":"workflow","workflowId":"nightly"}),30,None),
        ];
        assert_eq!(revocable(&grants).iter().map(|grant|grant["id"].as_str().unwrap()).collect::<Vec<_>>(),["workflow","session"]);
        assert_eq!(saved_lifetime(&grants[3]),"Named workflow nightly");assert_eq!(saved_lifetime(&grants[1]),"Connection session");
        let names=BTreeMap::from([("bpm-directives".to_owned(),"BPM directives".to_owned())]);
        assert_eq!(grant_allows(&grants[1],&names),"Temporary helpers and temporary security group membership for BPM directives; temporary server export files for report-definitions. Account operator in company C at https://example.invalid/Epicor/.");
    }
    #[test]
    fn utc_time_formats_civil_dates() {
        assert_eq!(utc_time(0),"1970-01-01 00:00 UTC");
        assert_eq!(utc_time(951_782_400),"2000-02-29 00:00 UTC");
        assert_eq!(utc_time(1_700_000_000),"2023-11-14 22:13 UTC");
    }
    fn visible_saved(app:&mut Gui,ctx:&egui::Context)->String {
        let mut labels=String::new();
        for _ in 0..3 {
            let mut output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1180.0,900.0))),..Default::default()},|ui|{app.saved_authorizations_window(ui.ctx());});
            output.textures_delta.clear();labels=output.shapes.into_iter().filter_map(|shape|if let egui::Shape::Text(text)=shape.shape {Some(text.galley.text().to_owned())}else{None}).collect::<Vec<_>>().join(" ");
        }labels
    }
    #[test]
    fn saved_authorizations_window_shows_rows_confirmation_empty_state_and_errors() {
        let ctx=egui::Context::default();let runtime=tokio::runtime::Runtime::new().unwrap();
        let mut app=super::super::gui_progress_layout_tests::app(&ctx,&runtime,false);
        app.saved_authorizations.open=true;app.saved_authorizations.grants=Some(vec![saved_grant("fixture-id",json!({"kind":"workflow","workflowId":"nightly"}),1_700_000_000,None)]);
        let text=visible_saved(&mut app,&ctx);
        for expected in ["Saved authorizations","Named workflow nightly","Temporary helpers","Created 2023-11-14 22:13 UTC · Authorization fixture-id","Revoke…","Refresh"] {assert!(text.contains(expected),"Missing: {expected}. Actual: {text}");}
        app.saved_authorizations.confirm=Some("fixture-id".into());
        let text=visible_saved(&mut app,&ctx);
        for expected in ["Revoke this authorization?","Revoke authorization","Keep it"] {assert!(text.contains(expected),"Missing: {expected}. Actual: {text}");}
        app.saved_authorizations.grants=Some(vec![]);
        assert!(visible_saved(&mut app,&ctx).contains("No saved authorizations."));
        app.saved_authorizations.grants=None;app.saved_authorizations.error=Some(Diagnostic::new("SAVED_FIXTURE","The saved authorizations could not be read."));
        let text=visible_saved(&mut app,&ctx);assert!(text.contains("The saved authorizations could not be read.")&&text.contains("SAVED_FIXTURE"),"{text}");
        assert!(!app.saved_authorizations.loading,"an error must not reload every frame");
    }
    #[test]
    fn saved_authorizations_list_and_revoke_through_the_host() {
        use crate::server_changes::{Lifetime,MaterialScope};
        let ctx=egui::Context::default();let runtime=tokio::runtime::Runtime::new().unwrap();let root=tempfile::tempdir().unwrap();
        let mut app=super::super::gui_progress_layout_tests::app(&ctx,&runtime,false);
        app.host=Host::new(root.path().join("state"),root.path().join("plugins"));
        let state=app.host.state_root.clone();
        let scope:MaterialScope=serde_json::from_value(json!({"endpoint":"https://example.invalid/Epicor/","company":"C","principal":"operator","capabilities":{"functions":{"binding":"fixture","selection":{},"effects":{"helper:reader":{"kind":"helper"}}}}})).unwrap();
        let session=crate::server_changes::create(&state,scope.clone(),Lifetime::Session{session_id:"fixture-session".into()},None,"interactiveGui").unwrap();
        crate::server_changes::create(&state,scope.clone(),Lifetime::OneRun{request_id:"fixture-run".into()},None,"interactiveGui").unwrap();
        let workflow=crate::server_changes::create(&state,scope,Lifetime::Workflow{workflow_id:"nightly".into()},None,"interactiveGui").unwrap();
        let settle=|app:&mut Gui| {
            // Local file operations settle well inside this; the limit only diagnoses a hang.
            let deadline=Instant::now()+Duration::from_secs(30);
            loop {
                if app.saved_authorizations.receive() {app.load_saved_authorizations(&ctx);}
                if !app.saved_authorizations.loading&&app.saved_authorizations.revoking.is_none() {break;}
                assert!(Instant::now()<deadline,"saved authorization request did not settle");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let ids=|app:&Gui|revocable(app.saved_authorizations.grants.as_deref().unwrap()).iter().map(|grant|grant["id"].as_str().unwrap().to_owned()).collect::<std::collections::BTreeSet<_>>();
        app.open_saved_authorizations();app.load_saved_authorizations(&ctx);settle(&mut app);
        assert_eq!(ids(&app),std::collections::BTreeSet::from([session.id.clone(),workflow.id.clone()]));
        app.revoke_saved_authorization(&ctx,session.id.clone());settle(&mut app);
        assert!(crate::server_changes::get(&state,&session.id).unwrap().revoked_at.is_some());
        assert!(app.saved_authorizations.notice.contains("Authorization revoked."));assert!(app.saved_authorizations.error.is_none());
        assert_eq!(ids(&app),std::collections::BTreeSet::from([workflow.id.clone()]));
        app.revoke_saved_authorization(&ctx,"missing-fixture".into());settle(&mut app);
        assert!(app.saved_authorizations.error.as_ref().is_some_and(|error|error.message.contains("does not exist")),"{:?}",app.saved_authorizations.error.as_ref().map(|error|&error.message));
        assert!(crate::server_changes::get(&state,&workflow.id).unwrap().revoked_at.is_none());
        assert!(runtime.block_on(app.host.shutdown()).is_empty());
    }

}
