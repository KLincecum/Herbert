//! Compact forms. Options are labeled and visible. There are no hidden expanders.
use super::*;
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool) {
    let label = ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .password(secret)
            .desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}
fn surface(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(18)
        .corner_radius(10)
}
impl Gui {
    pub(super) fn plain_text_file_help(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let mut open = self.file_help_open;
        let available = ctx.content_rect().size() - egui::vec2(48.0, 64.0);
        egui::Window::new("Use a plain-text credentials file")
            .open(&mut open)
            .default_size(egui::vec2(700.0, 720.0).min(available))
            .max_size(available)
            .resizable(true)
            .vscroll(false)
            .show(ctx, |ui| {
                if ui.add_enabled(self.editable(), egui::Button::new("Choose my credentials file…")).clicked() {
                    self.source = Source::File;
                    self.tab = Tab::Connection;
                    self.file_help_open = false;
                    self.pick_path(Picker::Input, ctx, frame);
                }
                ui.separator();
                // Keep the file action visible while the longer instructions scroll.
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                ui.label("Herbert supports credentials in a JSON file you create manually. It never creates, exports, copies, or edits that file.");
                ui.add_space(6.0);
                ui.label("1. Start a text editor, such as Notepad.");
                ui.label("Copy the example. Replace each placeholder with your connection details.");
                ui.label("2. Save the file with UTF-8 encoding. Name it herbert-environments.json.");
                ui.label("In Notepad, select All files. This prevents a .txt file name extension.");
                ui.label("3. On Connection, select Plain-text credentials file. Select Choose file.");
                ui.label("Select the environment by name. Select Save to remember the file location.");
                ui.label("Go to Download to start a download.");
                ui.label("Anyone with file access can read the password. Store the file in a private location.");
                ui.label("Store the file outside the download folder and source control.");
                ui.add_space(6.0);
                if ui.button("Copy example (placeholders only)").clicked() {
                    ctx.copy_text(CREDENTIAL_FILE_EXAMPLE.into());
                }
                ui.label(RichText::new(CREDENTIAL_FILE_EXAMPLE).monospace().size(13.0));
                ui.label("You can also use a full browser URL. Herbert removes browser and API suffixes. It keeps capitalization in the instance path.");
                ui.label("For Basic authentication with an API key, set authMode to basicApiKey. Replace null with the quoted API key.");
                ui.label("For a managed session, set authMode to session. Set an API key only when the server requires one.");
                ui.label(r#"Do not remove the JSON quotes or commas. Inside a value, write a double quote as \". Write a backslash as \\."#);
                ui.label("Give each additional environment a unique id.");

                    });
            });
        self.file_help_open &= open;
    }
    pub(super) fn connection_form(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        if !self.profiles_loaded || !self.settings_loaded {
            if self.diagnostic.is_none() {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Loading saved environments and settings…"); });
            } else if ui.button("Retry loading saved environments and settings").clicked() {
                self.notice.clear();
                self.diagnostic = None;
                if !self.profiles_loaded { self.send("profiles.list", json!({}), ui.ctx()); }
                if !self.settings_loaded { self.send("settings.inspect", json!({}), ui.ctx()); }
            }
        }
        let editable = self.editable();
        surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_enabled_ui(editable, |ui| {
                ui.heading("Connection");
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Environment");
                    let selected = self
                        .profiles
                        .iter()
                        .find(|p| p.id == self.profile_id)
                        .map(|p| p.name.as_deref().unwrap_or(&p.id))
                        .unwrap_or("New environment")
                        .to_owned();
                    egui::ComboBox::from_id_salt("saved-environment")
                        .width(250.0)
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(self.profile_id.is_empty(), "New environment")
                                .clicked()
                            {
                                self.select_profile(None, ui.ctx());
                            }
                            for profile in self.profiles.clone() {
                                if ui
                                    .selectable_label(
                                        self.profile_id == profile.id,
                                        profile.name.as_deref().unwrap_or(&profile.id),
                                    )
                                    .clicked()
                                {
                                    self.select_profile(Some(profile), ui.ctx());
                                }
                            }
                        });
                });
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.source, Source::Entered, "Enter credentials");
                    ui.add_enabled_ui(!self.profile_id.is_empty(), |ui| {
                        ui.radio_value(&mut self.source, Source::Saved, "Saved credentials");
                    });
                    ui.radio_value(&mut self.source, Source::File, "Plain-text credentials file");
                    if ui.button("How to use a credentials file…").clicked() {
                        self.file_help_open = true;
                    }
                });
                ui.add_space(6.0);
                if self.source == Source::File {
                    self.input_file_form(ui, frame);
                } else if ui.available_width() >= 640.0 {
                    ui.columns(2, |columns| {
                        self.environment_fields(&mut columns[0]);
                        self.credential_fields(&mut columns[1]);
                    });
                } else {
                    self.environment_fields(ui);
                    ui.add_space(8.0);
                    self.credential_fields(ui);
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.settings_loaded,
                            egui::Button::new(RichText::new(if self.source == Source::Entered && !self.profile_id.is_empty() { "Save updated credentials" } else { "Save" }).strong())
                                .min_size(egui::vec2(96.0, 34.0)),
                        )
                        .clicked()
                    {
                        self.save_environment(ui.ctx());
                    }
                    ui.label(
                        RichText::new(if self.source == Source::File {
                            "Save this environment file reference."
                        } else {
                            "Save the environment and credentials securely on this device."
                        })
                        .weak(),
                    );
                });
            });
        });
        if self.working || self.picker_pending {
            ui.horizontal(|ui| { ui.spinner(); ui.label(&self.notice); });
        } else if !self.notice.is_empty() {
            ui.label(RichText::new(&self.notice).color(if self.diagnostic.is_some() {
                ui.visuals().error_fg_color
            } else { ui.visuals().text_color() }));
        }
        if let Some(code) = &self.diagnostic { ui.label(RichText::new(code).small()); }
    }
    pub(super) fn download_form(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let actions = self.download_content(ui);
        if actions.choose_folder { self.pick_path(Picker::Destination, ui.ctx(), frame); }
        if actions.download { self.start_download(ui.ctx()); }
    }
    pub(super) fn download_content(&mut self, ui: &mut egui::Ui) -> DownloadActions {
        if !self.run_id.is_empty() {
            self.download_progress_panel(ui);
            return DownloadActions::default();
        }
        let editable = self.editable();
        let previous_method = self.collection_method;
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Prepare download");
                ui.add(egui::Label::new(format!("{} · {}", if self.name.is_empty() { "Select an environment on Connection" } else { &self.name },self.company)).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| self.sound_control(ui));
            });
            ui.add_space(4.0);
            ui.columns(2, |columns| {
                for (column, method) in columns.iter_mut().zip([
                    herbert_contract::CollectionMethod::DirectOnly,
                    herbert_contract::CollectionMethod::ServerAssisted,
                ]) {
                    egui::Frame::group(column.style()).inner_margin(8).corner_radius(6).show(column, |ui| {
                        ui.set_width(ui.available_width());
                        ui.radio_value(&mut self.collection_method,method,RichText::new(method.label()).strong());
                        let summary = if method == herbert_contract::CollectionMethod::DirectOnly {
                            "Existing Epicor services. No helpers."
                        } else { "Native calls, with helpers when needed or useful." };
                        ui.add(egui::Label::new(RichText::new(summary).size(14.0)).truncate()).on_hover_text(method.description());
                    });
                }
            });
        });
        ui.add_space(8.0);
        if self.collection_method != previous_method {
            if let Err(error) = crate::collection_preferences::write(&self.host.state_root,
                &crate::collection_preferences::Preferences { collection_method: self.collection_method }) {
                self.collection_method = previous_method;
                self.show_error(error);
            }
        }
        let actions=download_settings(ui, DownloadForm {
            destination: &mut self.destination,
            output_mode: &mut self.output_mode,
            temporary_access: &mut self.temporary_access,
            settings: &mut self.settings,
            editable,
            ready: self.settings_loaded && self.capabilities_loaded,
            selected: self.categories.values().filter(|selected| **selected).count(),
            busy: self.working || self.picker_pending,
            notice: &self.notice,
            diagnostic: self.diagnostic.as_deref(),
        });
        if self.diagnostic.is_some() && ui.add_enabled(!self.diagnostic_export_pending,egui::Button::new("Save diagnostics…")).on_hover_text("Save a sanitized packet without credentials, source or business values.").clicked() {
            self.save_diagnostics(ui.ctx());
        }
        actions
    }
    fn sound_control(&mut self, ui: &mut egui::Ui) {
        #[cfg(windows)]
        if ui.checkbox(&mut self.preferences.completion_sound, "Sounds")
            .on_hover_text("Play a sound after each batch and at completion. This setting is saved. CLI and MCP stay silent.")
            .changed() {
            self.completion_sound.set_enabled(self.preferences.completion_sound);
        }
    }
    pub(super) fn download_progress_panel(&mut self, ui: &mut egui::Ui) {
        surface(ui).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let method = self.journal.as_ref().and_then(|journal|
                serde_json::from_value::<herbert_contract::CollectionMethod>(journal["collectionMethod"].clone()).ok())
                .unwrap_or(self.collection_method);
            let environment = if self.name.is_empty() { "Selected environment" } else { &self.name };
            let context = [environment, self.company.as_str(), method.label()]
                .into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" / ");
            ui.horizontal(|ui| {
                ui.heading("Download monitor");
                ui.add(egui::Label::new(&context).truncate()).on_hover_text(&context);
                self.sound_control(ui);
            });
            ui.add_space(4.0);
            self.progress(ui);
        });
    }
    fn environment_fields(&mut self, ui: &mut egui::Ui) {
        field(ui, "Environment name", &mut self.name, false);
        field(ui, "Epicor instance URL", &mut self.url, false);
        ui.label(RichText::new("Examples:").small().weak());
        ui.label(
            RichText::new(
                "https://erp.example.com/Kinetic\nhttps://erp.example.com:8443/KineticTest",
            )
            .small()
            .weak(),
        );
        ui.label(RichText::new("Paste the full browser URL or just the instance URL. Herbert removes /apps/ERP or /api/v2 and anything after it, regardless of case, plus ? and # navigation details. Instance path capitalization is preserved.").small().weak());
        field(ui, "Company ID", &mut self.company, false);
        ui.label("Authentication");
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut self.mode, AuthMode::Session, "Managed session");
            ui.radio_value(&mut self.mode, AuthMode::BasicApiKey, "Basic + API key");
        });
    }
    fn credential_fields(&mut self, ui: &mut egui::Ui) {
        if self.source == Source::Saved {
            ui.label(RichText::new("Use this environment's saved credentials").strong());
            ui.label(match self.credential_status.as_deref() {
                Some("saved") => "Credentials are saved securely on this device.",
                Some(_) => {
                    "No saved credentials are available. Choose Enter credentials, then Save."
                }
                None => "Checking this environment's saved credentials…",
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Edit credentials").clicked() {
                    self.clear_secrets();
                    self.username.clear();
                    self.source = Source::Entered;
                    self.notice = "Enter the replacement username, password and API key, then click Save updated credentials. Existing credentials stay saved until you save the replacement.".into();
                    self.diagnostic = None;
                }
                if ui.add_enabled(self.credential_status.as_deref() == Some("saved"), egui::Button::new("Delete saved credentials")).clicked() {
                    self.working = true;
                    self.notice = "Deleting saved credentials from this device…".into();
                    self.diagnostic = None;
                    self.send("credentials.forget", json!({"id":self.profile_id}), ui.ctx());
                }
            });
            ui.label(RichText::new("Edit replaces the saved sign-in after you save. Delete removes credentials from this device and keeps the environment. It does not change your Epicor account or a credentials file.").small().weak());
        } else {
            field(ui, "Username", &mut self.username, false);
            field(ui, "Password", &mut self.password, true);
            field(
                ui,
                if self.mode == AuthMode::BasicApiKey {
                    "API key"
                } else {
                    "API key (if required)"
                },
                &mut self.api_key,
                true,
            );
        }
        ui.label(RichText::new("A Security Manager account is required. Download does not save newly entered credentials; use Save for that.").small());
    }
    fn input_file_form(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        ui.label("Use a JSON credentials file you created yourself. Herbert reads it; it never creates or changes it.");
        let input_label = ui.label("Credentials file (.json)");
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !self.input_file.trim().is_empty(),
                        egui::Button::new("Read file"),
                    )
                    .clicked()
                {
                    self.load_file_choices(ui.ctx());
                }
                if ui.button("Choose file…").clicked() {
                    self.pick_path(Picker::Input, ui.ctx(), frame);
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.input_file)
                        .desired_width(ui.available_width()),
                )
                .labelled_by(input_label.id);
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Named environment");
            let selected = self
                .file_choices
                .iter()
                .find(|p| p.id == self.file_environment)
                .map(|p| p.name.as_deref().unwrap_or(&p.id))
                .unwrap_or("Choose an environment")
                .to_owned();
            egui::ComboBox::from_id_salt("file-environment")
                .width(280.0)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for profile in self.file_choices.clone() {
                        if ui
                            .selectable_value(
                                &mut self.file_environment,
                                profile.id.clone(),
                                format!(
                                    "{} ({})",
                                    profile.name.as_deref().unwrap_or(&profile.id),
                                    profile.id
                                ),
                            )
                            .changed()
                        {
                            self.apply_file_metadata();
                        }
                    }
                });
            ui.checkbox(&mut self.file_company_override, "Override company");
        });
        field(ui, "Environment name", &mut self.name, false);
        ui.label(format!("URL: {}", self.url));
        ui.label(format!(
            "Authentication: {}",
            if self.mode == AuthMode::Session {
                "Managed session"
            } else {
                "Basic + API key"
            }
        ));
        if self.file_company_override {
            field(ui, "Company ID", &mut self.company, false);
        } else {
            ui.label(format!("Company: {}", self.company));
        }
        ui.label(RichText::new("The selected file supplies credentials and named environment details. Herbert reads it without changing it.").small());
    }
    pub(super) fn plugins_form(&mut self, ui: &mut egui::Ui) {
        ui.heading("Plugins");
        ui.label(
            RichText::new("Choose what to download and set each plugin's options here.").weak(),
        );
        ui.label(RichText::new("Category toggles, Select all available, Clear selection and Save plugin options save the current options too. Clear a filter and save to remove its saved value. Downloads use the displayed options.").small().weak());
        ui.add_space(8.0);
        let editable = self.editable();
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Select all available").clicked() {
                    for category in self.capabilities.clone() {
                        if category["available"].as_bool() == Some(true) {
                            if let Some(id) = category["id"].as_str() {
                                self.categories.insert(id.into(), true);
                                self.save_plugin_selection(id, ui.ctx());
                            }
                        }
                    }
                }
                if ui.button("Clear selection").clicked() {
                    for id in self.categories.keys().cloned().collect::<Vec<_>>() {
                        self.categories.insert(id.clone(), false);
                        self.save_plugin_selection(&id, ui.ctx());
                    }
                }
            });
            if !self.capabilities_loaded { ui.spinner(); ui.label("Loading installed plugins…"); }
            for category in self.capabilities.clone() {
                let Some(id) = category["id"].as_str() else { continue; };
                let available = category["available"].as_bool() == Some(true);
                ui.add_space(10.0);
                ui.push_id(id, |ui| {
                    let selected = self.categories.get(id).copied().unwrap_or(false);
                    let accent = if ui.visuals().dark_mode { egui::Color32::from_rgb(84, 190, 255) } else { egui::Color32::from_rgb(0, 91, 166) };
                    let card = if selected {
                        surface(ui).stroke(egui::Stroke::new(2.0, accent))
                            .fill(if ui.visuals().dark_mode { egui::Color32::from_rgb(24, 48, 68) } else { egui::Color32::from_rgb(231, 244, 255) })
                    } else { surface(ui) };
                    card.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                        ui.scope(|ui| {
                        ui.spacing_mut().icon_width = 24.0;
                        ui.spacing_mut().icon_width_inner = 16.0;
                        ui.visuals_mut().widgets.inactive.fg_stroke = egui::Stroke::new(2.0, if selected { accent } else { ui.visuals().text_color() });
                        if ui.add_enabled(available, egui::Checkbox::new(
                            self.categories.entry(id.into()).or_insert(false),
                            RichText::new(category["name"].as_str().unwrap_or(id)).strong().size(17.0))).changed() {
                            self.save_plugin_selection(id, ui.ctx());
                        }
                        });
                        let selected_now = self.categories.get(id).copied().unwrap_or(false);
                        egui::Frame::new().fill(if selected_now { accent } else { ui.visuals().extreme_bg_color }).inner_margin(egui::Margin::symmetric(10, 4)).corner_radius(5).show(ui, |ui| {
                            ui.label(RichText::new(if selected_now { "SELECTED" } else if available { "Not selected" } else { "Unavailable" }).strong().color(if selected_now { if ui.visuals().dark_mode { egui::Color32::from_rgb(10, 24, 36) } else { egui::Color32::WHITE } } else { ui.visuals().weak_text_color() }));
                        });
                        });
                        ui.indent("plugin-body", |ui| {
                            crate::gui::gui_plugin_settings::helper_label(ui, &category, self.collection_method);
                        if let Some(description) = category["description"].as_str() { ui.label(RichText::new(description).italics()); }
                        if !available { ui.label(RichText::new("Not available for download. See the plugin options below.").small().color(ui.visuals().warn_fg_color)); }
                        if let Ok(artifacts) = serde_json::from_value::<Vec<herbert_contract::NativeArtifactDeclaration>>(category["artifacts"].clone()) {
                            if !artifacts.is_empty() { ui.add_space(6.0); ui.label(RichText::new("Native artifacts").strong()); }
                            for artifact in artifacts {
                                ui.label(&artifact.effects.description);
                                let lifecycle = match artifact.lifetime {
                                    herbert_contract::NativeArtifactLifetime::Temporary => "Removed when this operation finishes.",
                                    herbert_contract::NativeArtifactLifetime::Persistent => "Kept in Epicor after this operation.",
                                };
                                ui.label(RichText::new(lifecycle).small().weak());
                                match artifact.policy {
                                    herbert_contract::NativeArtifactPolicy::CreateInstance => { ui.label(RichText::new("Installs a separate native instance.").small().weak()); }
                                    herbert_contract::NativeArtifactPolicy::UpdateOwned => { ui.label(RichText::new(format!("Updates Herbert's installation: {}.", artifact.target.as_deref().unwrap_or(""))).small().weak()); }
                                    herbert_contract::NativeArtifactPolicy::ReplaceNamed => { ui.label(RichText::new(format!("Replaces {} after preserving its native export.", artifact.target.as_deref().unwrap_or(""))).small()); }
                                }
                                if artifact.effects.submits_updates { ui.label(RichText::new("This plugin can submit updates.").strong()); }
                                if artifact.effects.invokes_custom_actions { ui.label(RichText::new("This plugin can invoke UBAQ custom actions.").strong()); }
                                ui.add_space(4.0);
                            }
                        }
                        if let Ok(sections) = serde_json::from_value::<Vec<herbert_contract::HelpSection>>(category["help"].clone()) {
                            for section in sections {
                                ui.add_space(8.0);
                                if !section.title.is_empty() { ui.label(RichText::new(&section.title).strong().size(15.0)); }
                                ui.indent(section.title, |ui| {
                                    for paragraph in section.paragraphs { ui.label(RichText::new(paragraph).weak()); }
                                });
                            }
                        }
                        if category["settings"].as_array().is_some_and(|settings| !settings.is_empty()) {
                            ui.add_space(10.0);
                            ui.separator();
                            ui.label(RichText::new("Options").strong().size(16.0));
                        }
                        gui_plugin_settings::show(ui, &category, self.plugin_settings.entry(id.into()).or_insert(json!({})));
                        if category["settings"].as_array().is_some_and(|settings| !settings.is_empty()) {
                            ui.add_space(6.0);
                            if ui.button("Save plugin options").clicked() {
                                self.save_plugin_selection(id, ui.ctx());
                            }
                        }
                        });
                    });
                });
            }
        });
        if self.diagnostic.is_some() {
            ui.label(RichText::new(&self.notice).color(ui.visuals().error_fg_color));
        }
    }
    fn save_plugin_selection(&mut self, id: &str, ctx: &egui::Context) {
        self.send("plugin.configure", json!({"category":id,"selected":self.categories.get(id).copied().unwrap_or(false),"values":self.plugin_settings.get(id).cloned().unwrap_or(json!({}))}), ctx);
    }
}

// Static placeholders only. Never interpolate a profile or entered credentials.
const CREDENTIAL_FILE_EXAMPLE: &str = r#"{
  "schemaVersion": 1,
  "environments": [{
    "id": "my-environment",
    "name": "My environment",
    "url": "https://erp.example.com/Kinetic",
    "authMode": "session",
    "credentials": {
      "username": "YOUR_USERNAME",
      "password": "YOUR_PASSWORD",
      "apiKey": null
    },
    "defaults": { "company": "YOUR_COMPANY" }
  }]
}"#;

// Render without host or persistence access so geometry can be checked headlessly.
struct DownloadForm<'a> {
    destination: &'a mut String,
    output_mode: &'a mut Option<bool>,
    temporary_access: &'a mut bool,
    settings: &'a mut SettingsForm,
    editable: bool,
    ready: bool,
    selected: usize,
    busy: bool,
    notice: &'a str,
    diagnostic: Option<&'a str>,
}
#[derive(Default)]
pub(super) struct DownloadActions { choose_folder: bool, download: bool }
fn download_settings(ui: &mut egui::Ui, form: DownloadForm<'_>) -> DownloadActions {
    let mut actions = DownloadActions::default();
    surface(ui).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add_enabled_ui(form.editable, |ui| {
            ui.label(RichText::new("Save files").strong().size(17.0));
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Choose folder…").clicked() { actions.choose_folder=true; }
                    ui.add(egui::TextEdit::singleline(&mut *form.destination)
                        .hint_text("Destination folder").desired_width(ui.available_width()));
                });
            });
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut *form.output_mode,Some(true),"New folder for this run");
                ui.radio_value(&mut *form.output_mode,Some(false),"Refresh the chosen folder");
            });
            ui.label(RichText::new("New folder keeps each run separate. Refresh updates the folder and preserves user edits.").small().weak());
        });
    });
    ui.add_space(8.0);
    surface(ui).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add_enabled_ui(form.editable, |ui| {
            ui.label(RichText::new("Temporary access").strong().size(17.0));
            ui.checkbox(&mut *form.temporary_access,"Allow Herbert to temporarily add my Epicor user to required security groups")
                .on_hover_text("This separate permission permits eligible group changes only with explicit scoped authorization. Herbert preserves preexisting memberships and other active owners.");
            ui.label(RichText::new("Herbert records temporary additions and verifies cleanup. Pending, failed or unknown cleanup remains visible.").small().weak());
        });
    });
    ui.add_space(8.0);
    egui::CollapsingHeader::new("Advanced").default_open(false).show(ui,|ui| {
        ui.add_enabled_ui(form.editable,|ui|form.settings.ui(ui));
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let button=egui::Button::new(RichText::new("Download").strong()
            .color(ui.visuals().selection.stroke.color))
            .fill(ui.visuals().selection.bg_fill).min_size(egui::vec2(140.0,36.0));
        if ui.add_enabled(form.editable && form.ready,button).clicked() { actions.download=true; }
        ui.label(format!("{} plugins selected · Review the selection on Plugins",form.selected));
        if form.busy { ui.spinner(); }
    });
    // Status changes below the action, never between the action and its options.
    if !form.notice.is_empty() {
        ui.label(RichText::new(form.notice).color(if form.diagnostic.is_some() {
            ui.visuals().error_fg_color
        } else { ui.visuals().text_color() }));
    }
    if let Some(code)=form.diagnostic { ui.label(RichText::new(code).small()); }
    actions
}

#[cfg(test)]
mod download_layout_tests {
    use super::*;
    fn measure(width: f32, height: f32, busy: bool, notice: &str, diagnostic: Option<&str>) -> (egui::Vec2, egui::Rect) {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut destination = String::new();
        let mut mode = Some(false);
        let mut access = true;
        let mut settings = SettingsForm::new(&Default::default());
        let before = serde_json::to_value(&settings).unwrap();
        let mut measured = None;
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))), ..Default::default() }, |ui| {
                egui::Panel::top("tabs-test").exact_size(60.0).show(ui, |_| {});
                egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(20)).show(ui, |ui| {
                    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let actions = download_settings(ui, DownloadForm {
                            destination: &mut destination, output_mode: &mut mode, temporary_access: &mut access,
                            settings: &mut settings, editable: !busy, ready: true, selected: 5,
                            busy, notice, diagnostic,
                        });
                        assert!(!actions.choose_folder && !actions.download);
                        ui.add_space(12.0);
                    });
                    measured = Some((scroll.content_size, scroll.inner_rect));
                });
            });
            // This headless geometry test has no renderer to consume texture updates.
            output.textures_delta.clear();
        }
        assert_eq!(serde_json::to_value(&settings).unwrap(), before);
        assert!(destination.is_empty());
        assert_eq!(mode, Some(false));
        assert!(access);
        measured.unwrap()
    }
    #[test]
    fn download_form_fits_idle_picker_and_notice_at_reported_size() {
        for (busy, notice, diagnostic) in [
            (false, "", None),
            (true, "", None),
            (true, "Choose a destination folder…", None),
            (false, "Choose a destination folder before starting the download.", Some("DESTINATION_REQUIRED")),
            (false, "Environment and credentials saved securely on this device. Download settings applied for this session.", None),
        ] {
            let (content, viewport) = measure(1180.0, 820.0, busy, notice, diagnostic);
            println!("busy={busy} notice={notice:?} content={content:?} viewport={:?}", viewport.size());
            assert!(content.y <= viewport.height(), "Download controls require scrolling: {content:?} in {viewport:?}");
            assert!(content.x <= viewport.width() + 1.0, "horizontal overflow");
        }
    }
    #[test]
    fn download_form_fits_wider_default() {
        let (content, viewport) = measure(1360.0, 900.0, false, "", None);
        assert!(content.y <= viewport.height(), "default controls require scrolling");
        assert!(content.x <= viewport.width() + 1.0, "horizontal overflow");
    }
    #[test]
    fn download_form_keeps_narrow_controls_in_vertical_scroll_content() {
        let (content, viewport) = measure(720.0, 560.0, true, "Choose a destination folder…", None);
        assert!(content.y > viewport.height());
        assert!(content.x <= viewport.width() + 1.0, "narrow horizontal overflow: {content:?} in {viewport:?}");
    }
}
