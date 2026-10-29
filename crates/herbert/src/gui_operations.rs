//! Save and Download are short sequences of existing shared Host operations.
use super::*;

async fn execute(host: &Arc<Host>, method: &str, arguments: Value) -> Reply {
    host.execute(Command {
        schema_version: 1,
        method: method.into(),
        arguments,
    })
    .await
}
fn failed(error: Diagnostic) -> Reply {
    Reply {
        schema_version: 1,
        ok: false,
        result: None,
        error: Some(error),
    }
}
impl Gui {
    fn credential_draft(&self) -> Result<Credentials, Diagnostic> {
        let value = Credentials {
            username: self.username.clone(),
            password: self.password.clone(),
            api_key: if self.api_key.is_empty() {
                None
            } else {
                Some(self.api_key.clone())
            },
        };
        value.validate(self.mode)?;
        Ok(value)
    }
    fn check_file_selection(&self) -> Result<(), Diagnostic> {
        if self.input_file != self.file_choices_path
            || !self
                .file_choices
                .iter()
                .any(|p| p.id == self.file_environment)
        {
            return Err(Diagnostic::new(
                "ENVIRONMENT_SELECTION_REQUIRED",
                "Choose the input file, then select one of its named environments.",
            ));
        }
        Ok(())
    }
    fn check_saved_endpoint(&self) -> Result<(), Diagnostic> {
        let profile = self
            .profiles
            .iter()
            .find(|p| p.id == self.profile_id)
            .ok_or_else(|| {
                Diagnostic::new(
                    "PROFILE_REQUIRED",
                    "Choose a saved environment, or enter credentials.",
                )
            })?;
        if crate::input::endpoint(&self.url)? != crate::input::endpoint(&profile.url)? {
            return Err(Diagnostic::new(
                "CREDENTIAL_ENDPOINT_MISMATCH",
                "The URL changed. Enter and save credentials for the new endpoint; saved credentials stay bound to the original URL.",
            ));
        }
        Ok(())
    }
    pub(super) fn save_environment(&mut self, ctx: &egui::Context) {
        if !self.editable() {
            return;
        }
        let preparation = (|| -> Result<(Profile, Option<Credentials>, herbert_contract::OperatingSettings), Diagnostic> {
            let settings = self.settings.value()?;
            match self.source {
                Source::File => self.check_file_selection()?,
                Source::Saved => self.check_saved_endpoint()?,
                Source::Entered => {}
            }
            let profile = Profile {
                id: if self.profile_id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    self.profile_id.clone()
                },
                name: Some(if self.name.trim().is_empty() {
                    self.url.trim().into()
                } else {
                    self.name.trim().into()
                }),
                url: crate::input::endpoint(&self.url)?.to_string(),
                auth_mode: self.mode,
                defaults: Defaults {
                    company: if self.company.trim().is_empty() {
                        None
                    } else {
                        Some(self.company.trim().into())
                    },
                    concurrency: Some(self.settings.concurrency),
                },
                user_file: if self.source == Source::File {
                    Some(UserFileRef {
                        path: self.input_file.clone(),
                        environment_id: self.file_environment.clone(),
                    })
                } else {
                    None
                },
                allow_untrusted_certificate: self.allow_untrusted_certificate,
            };
            crate::input::validate_profile(&profile)?;
            let credentials = if self.source == Source::Entered {
                Some(self.credential_draft()?)
            } else {
                None
            };
            Ok((profile, credentials, settings))
        })();
        let (profile, credentials, settings) = match preparation {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        self.url = profile.url.clone();
        self.working = true;
        self.notice = "Saving environment and credentials…".into();
        self.diagnostic = None;
        let host = self.host.clone();
        let sender = self.sender.clone();
        let source = self.source;
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let mut reply = execute(&host, "profiles.upsert", json!(profile)).await;
            let profile_saved = reply.ok;
            let mut credentials_saved = false;
            if reply.ok {
                if let Some(credentials) = credentials {
                    reply = execute(&host, "credentials.save", json!({"id":profile.id,"credentials":{"username":credentials.username,"password":credentials.password,"apiKey":credentials.api_key}})).await;
                    credentials_saved = reply.ok;
                    if let Some(error) = reply.error.as_mut() {
                        error.message = format!("Environment details were saved, but credentials were not saved and download settings were not applied. {}", error.message);
                    }
                } else if source == Source::Saved {
                    reply = execute(&host, "credentials.status", json!({"id":profile.id})).await;
                    if reply.ok && reply.result.as_ref().and_then(|v| v["status"].as_str()) != Some("saved") {
                        reply = failed(Diagnostic::new("CREDENTIALS_REQUIRED", "No secure credentials are available. Choose Enter credentials, then Save."));
                    }
                    if let Some(error) = reply.error.as_mut() {
                        error.message = format!("Environment details were saved, but saved credentials could not be confirmed and download settings were not applied. {}", error.message);
                    }
                    credentials_saved = reply.ok;
                }
            }
            if reply.ok {
                reply = execute(&host, "settings.update", json!(settings)).await;
                if let Some(error) = reply.error.as_mut() {
                    let saved = if source == Source::File { "The environment file reference was saved" } else { "The environment and secure credentials were saved" };
                    error.message = format!("{saved}, but download settings were not applied. {}", error.message);
                }
            }
            // Typed credentials zeroize on drop. Replies never contain secrets.
            let _ = sender.send(Event::Saved { profile: profile.id, source, profile_saved, credentials_saved, reply });
            ctx.request_repaint();
        });
    }
    pub(super) fn connection_arguments(
        &self,
        settings: &herbert_contract::OperatingSettings,
    ) -> Result<Value, Diagnostic> {
        let source = match self.source {
            Source::File => {
                self.check_file_selection()?;
                json!({"type":"userFile","path":self.input_file,"environmentId":self.file_environment})
            }
            Source::Saved => {
                self.check_saved_endpoint()?;
                json!({"type":"secureStore"})
            }
            Source::Entered => {
                let credentials = self.credential_draft()?;
                json!({"type":"inline","credentials":{"username":credentials.username,"password":credentials.password,"apiKey":credentials.api_key}})
            }
        };
        Ok(json!({
            "profileId":if self.source==Source::Saved{Some(&self.profile_id)}else{None},
            "url":if self.source==Source::File{None}else{Some(&self.url)},
            "authMode":if self.source==Source::File{None}else{Some(self.mode)},
            "company":if self.source==Source::File&&!self.file_company_override{None}else{Some(&self.company)},
            "settings":settings,"credentialSource":source,
            "allowUntrustedCertificate":self.allow_untrusted_certificate
        }))
    }
    /// A download needs a company; the Connection screen explains how to choose one.
    pub(super) fn company_missing(&self) -> bool {
        self.company.trim().is_empty() && !(self.source == Source::File && !self.file_company_override
            && self.file_choices.iter().any(|p| p.id == self.file_environment && p.defaults.company.as_deref().is_some_and(|c| !c.is_empty())))
    }
    pub(super) fn start_download(&mut self, ctx: &egui::Context) {
        if !self.editable() {
            return;
        }
        let preparation =
            (|| -> Result<(Value, Value, herbert_contract::OperatingSettings), Diagnostic> {
                let settings = self.settings.value()?;
                let connection = self.connection_arguments(&settings)?;
                let categories = self
                    .categories
                    .iter()
                    .filter(|(_, selected)| **selected)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                if categories.is_empty() {
                    return Err(Diagnostic::new(
                        "COLLECTION_REQUIRED",
                        "Select at least one collection on the Plugins tab.",
                    ));
                }
                if self.destination.trim().is_empty() {
                    return Err(Diagnostic::new(
                        "DESTINATION_REQUIRED",
                        "Choose a destination folder.",
                    ));
                }
                let separate = self.output_mode.ok_or_else(|| {
                    Diagnostic::new(
                        "OUTPUT_MODE_REQUIRED",
                        "Choose Separate folder or Refresh existing folder.",
                    )
                })?;
                let mut request = json!({"requestId":uuid::Uuid::new_v4().to_string(),"destination":self.destination,"outputMode":if separate{"separate"}else{"refresh"},"collectionMethod":self.collection_method,"categories":categories,"pageSize":settings.page_size,"temporaryAccess":self.temporary_access,"pluginSettings":self.plugin_settings});
                if self.authorization_choice == super::gui_server_changes::AuthorizationChoice::Workflow && !self.authorization_workflow.trim().is_empty() { request["workflowId"] = json!(self.authorization_workflow); }
                Ok((connection, request, settings))
            })();
        let (connection, request, settings) = match preparation {
            Ok(value) => value,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let readiness_key = self.account_key().ok();
        let connection_key = super::gui_server_changes::connection_key(&connection, true);
        let reuse = self.connected && !self.cleanup_pending && self.connected_key.as_deref() == Some(connection_key.as_str());
        self.run_id.clear();self.journal=None;self.output_root.clear();self.selected_authorization=None;
        self.working=true;self.notice="Preparing read-only preview…".into();self.diagnostic=None;
        let host=self.host.clone();let sender=self.sender.clone();
        let connected=cleanup_before_login(self.connected,self.cleanup_pending);let ctx=ctx.clone();
        self.runtime.spawn(async move {
            let result=async {
                let settings=execute(&host,"settings.update",json!(settings)).await;
                if !settings.ok {return Some(settings);}
                if !reuse {
                    if connected {
                        let disconnected=execute(&host,"connection.disconnect",json!({})).await;
                        if !disconnected.ok {return Some(disconnected);}
                        let _=sender.send(Event::Connection{active:false,key:None});
                    }
                    let _=sender.send(Event::Stage("Connecting to Epicor…"));ctx.request_repaint();
                    let connected=execute(&host,"connection.connect",connection).await;
                    if let Some(key)=readiness_key {
                        let reply=Reply{schema_version:connected.schema_version,ok:connected.ok,result:connected.result.clone(),error:connected.error.clone()};
                        let _=sender.send(Event::AccountReadiness{key,reply,testing:false});
                    }
                    if !connected.ok {return Some(connected);}
                    let _=sender.send(Event::Connection{active:true,key:Some(connection_key.clone())});
                }
                let _=sender.send(Event::Stage("Preparing read-only preview…"));ctx.request_repaint();
                Self::preview_or_start(&host,&sender,request,connection_key).await
            }.await;
            if let Some(reply)=result {let _=sender.send(Event::Started(reply));}
            ctx.request_repaint();
        });
    }
}

impl Gui {
    pub(super) fn save_diagnostics(&mut self,ctx:&egui::Context) {
        if self.diagnostic_export_pending{return;}
        self.diagnostic_export_pending=true;
        let future=rfd::AsyncFileDialog::new().set_title("Save sanitized diagnostic packet")
            .add_filter("Diagnostic ZIP",&["zip"]).set_file_name("Herbert-diagnostics.zip").save_file();
        let host=self.host.clone();let sender=self.sender.clone();let ctx=ctx.clone();let id=(!self.run_id.is_empty()).then(||self.run_id.clone());
        self.runtime.spawn(async move {
            let reply=if let Some(file)=future.await {
                execute(&host,"diagnostics.export",json!({"id":id,"path":file.path()})).await
            } else {Reply{schema_version:1,ok:false,result:None,error:None}};
            let _=sender.send(Event::DiagnosticExported(reply));ctx.request_repaint();
        });
    }
}
