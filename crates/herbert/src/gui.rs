//! Direct tabs over shared Host operations: Connection, Download, Plugins and About.
use crate::{
    application::{Command, Host, Reply},
    input::{AuthMode, Credentials, Defaults, Profile, UserFileRef},
};
#[path = "gui_about.rs"]
mod gui_about;
#[path = "gui_form.rs"]
mod gui_form;
#[path = "gui_operations.rs"]
mod gui_operations;
#[path = "gui_plugin_settings.rs"]
mod gui_plugin_settings;
#[path = "gui_progress.rs"]
mod gui_progress;
#[path = "gui_settings.rs"]
mod gui_settings;
#[path = "gui_preferences.rs"]
mod gui_preferences;
#[path = "gui_sound.rs"]
mod gui_sound;
use eframe::egui::{self, RichText};
use gui_settings::SettingsForm;
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use zeroize::Zeroize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Connection,
    Download,
    Plugins,
    About,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Entered,
    Saved,
    File,
}
#[derive(Clone, Copy)]
enum Picker {
    Input,
    Destination,
}
enum Event {
    DiagnosticExported(Reply),
    Reply {
        method: String,
        generation: u64,
        profile: String,
        reply: Reply,
    },
    Saved {
        profile: String,
        source: Source,
        profile_saved: bool,
        credentials_saved: bool,
        reply: Reply,
    },
    Started(Reply),
    Connection(bool),
    Stage(&'static str),
    FileChoices {
        generation: u64,
        path: String,
        result: Result<Vec<Profile>, Diagnostic>,
    },
    Picked {
        picker: Picker,
        path: Option<std::path::PathBuf>,
    },
}
struct Gui {
    host: Arc<Host>,
    runtime: tokio::runtime::Handle,
    sender: mpsc::Sender<Event>,
    receiver: mpsc::Receiver<Event>,
    tab: Tab,
    about: gui_about::AboutPanel,
    brand_icon: egui::TextureHandle,
    settings: SettingsForm,
    settings_loaded: bool,
    capabilities_loaded: bool,
    working: bool,
    picker_pending: bool,
    diagnostic_export_pending: bool,
    generation: u64,
    notice: String,
    diagnostic: Option<String>,
    profiles: Vec<Profile>,
    profile_id: String,
    name: String,
    url: String,
    company: String,
    username: String,
    password: String,
    api_key: String,
    mode: AuthMode,
    source: Source,
    credential_status: Option<String>,
    input_file: String,
    file_environment: String,
    file_choices: Vec<Profile>,
    file_choices_path: String,
    file_company_override: bool,
    file_help_open: bool,
    preferences: gui_preferences::Preferences,
    completion_sound: gui_sound::CompletionSound,
    preferences_ready: bool,
    profiles_loaded: bool,
    preferences_bytes: Vec<u8>,
    preferences_error: Option<String>,
    capabilities: Vec<Value>,
    categories: BTreeMap<String, bool>,
    plugin_settings: BTreeMap<String, Value>,
    destination: String,
    output_mode: Option<bool>,
    collection_method: herbert_contract::CollectionMethod,
    temporary_access: bool,
    connected: bool,
    cleanup_pending: bool,
    run_id: String,
    output_root: String,
    folder_open_error: Option<String>,
    journal: Option<Value>,
    poll_pending: bool,
    last_poll: Instant,
}
impl Drop for Gui {
    fn drop(&mut self) {
        self.password.zeroize();
        self.api_key.zeroize();
    }
}

pub fn launch(host: Arc<Host>, runtime: tokio::runtime::Handle) -> Result<(), eframe::Error> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!(
        "../../../assets/branding/herbert-vacuum.png"
    ))
    .map_err(|error| eframe::Error::AppCreation(Box::new(error)))?;
    eframe::run_native(
        "Herbert",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_icon(icon.clone())
                .with_inner_size([1360.0, 900.0])
                .with_min_inner_size([720.0, 560.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            configure_style(&cc.egui_ctx);
            let (sender, receiver) = mpsc::channel();
            let brand_icon = cc.egui_ctx.load_texture(
                "herbert-vacuum",
                egui::ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            let mut app = Gui {
                host,
                runtime,
                sender,
                receiver,
                tab: Tab::Connection,
                about: Default::default(),
                brand_icon,
                settings: SettingsForm::new(&Default::default()),
                settings_loaded: false,
                capabilities_loaded: false,
                working: false,
                picker_pending: false,
                diagnostic_export_pending: false,
                generation: 0,
                notice: String::new(),
                diagnostic: None,
                profiles: vec![],
                profile_id: String::new(),
                name: String::new(),
                url: String::new(),
                company: String::new(),
                username: String::new(),
                password: String::new(),
                api_key: String::new(),
                mode: AuthMode::Session,
                source: Source::Entered,
                credential_status: None,
                input_file: String::new(),
                file_environment: String::new(),
                file_choices: vec![],
                file_choices_path: String::new(),
                file_company_override: false,
                file_help_open: false,
                preferences: Default::default(),
                completion_sound: Default::default(),
                preferences_ready: false,
                profiles_loaded: false,
                preferences_bytes: vec![],
                preferences_error: None,
                capabilities: vec![],
                categories: BTreeMap::new(),
                plugin_settings: BTreeMap::new(),
                destination: String::new(),
                output_mode: Some(true),
                collection_method: herbert_contract::CollectionMethod::DirectOnly,
                temporary_access: crate::application::default_temporary_access(),
                connected: false,
                cleanup_pending: false,
                run_id: String::new(),
                output_root: String::new(),
                folder_open_error: None,
                journal: None,
                poll_pending: false,
                last_poll: Instant::now(),
            };
            app.load_preferences(&cc.egui_ctx);
            for method in ["profiles.list", "capabilities", "settings.inspect"] {
                app.send(method, json!({}), &cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}
impl Gui {
    fn send(&mut self, method: &str, arguments: Value, ctx: &egui::Context) {
        if method == "run.status" {
            if self.poll_pending {
                return;
            }
            self.poll_pending = true;
        }
        let host = self.host.clone();
        let sender = self.sender.clone();
        let method = method.to_owned();
        let generation = self.generation;
        let profile = self.profile_id.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let reply = host
                .execute(Command {
                    schema_version: 1,
                    method: method.clone(),
                    arguments,
                })
                .await;
            let _ = sender.send(Event::Reply {
                method,
                generation,
                profile,
                reply,
            });
            ctx.request_repaint();
        });
    }
    fn show_error(&mut self, error: Diagnostic) {
        apply_connection_error(&error, &mut self.connected, &mut self.cleanup_pending);
        self.notice = connection_error_message(&error);
        self.diagnostic = Some(error.code);
    }
    fn clear_secrets(&mut self) {
        self.password.zeroize();
        self.password.clear();
        self.api_key.zeroize();
        self.api_key.clear();
    }
    fn run_active(&self) -> bool {
        !self.run_id.is_empty()
            && self
                .journal
                .as_ref()
                .is_none_or(gui_progress::execution_active)
    }
    fn editable(&self) -> bool {
        self.profiles_loaded && self.settings_loaded
            && !self.working && !self.run_active() && !self.picker_pending
    }
    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                Event::DiagnosticExported(reply) => {
                    self.diagnostic_export_pending=false;
                    if reply.ok { self.notice="Sanitized diagnostic packet saved. No source, business values or credentials included.".into();self.diagnostic=None; }
                    else if let Some(error)=reply.error {self.show_error(error);}
                }
                Event::Stage(message) => {
                    self.notice = message.into();
                    self.diagnostic = None;
                }
                Event::Connection(active) => {
                    self.connected = active;
                    self.cleanup_pending = false;
                },
                Event::Picked { picker, path } => {
                    self.picker_pending = false;
                    if let Some(path) = path {
                        match path.into_os_string().into_string() {
                            Ok(path) => match picker {
                                Picker::Input => {
                                    self.input_file = path;
                                    self.load_file_choices(ctx);
                                }
                                Picker::Destination => self.destination = path,
                            },
                            Err(_) => self.show_error(Diagnostic::new(
                                "PATH_ENCODING",
                                "Choose a path that can be represented as UTF-8.",
                            )),
                        }
                    }
                }
                Event::FileChoices {
                    generation,
                    path,
                    result,
                } => {
                    if generation != self.generation || path != self.input_file {
                        continue;
                    }
                    match result {
                        Ok(choices) => {
                            self.file_choices = choices;
                            self.file_choices_path = path;
                            if !self
                                .file_choices
                                .iter()
                                .any(|p| p.id == self.file_environment)
                            {
                                self.file_environment = if self.file_choices.len() == 1 {
                                    self.file_choices[0].id.clone()
                                } else {
                                    String::new()
                                };
                            }
                            self.apply_file_metadata();
                            self.notice = format!(
                                "{} named environments in this file.",
                                self.file_choices.len()
                            );
                            self.diagnostic = None;
                        }
                        Err(error) => self.show_error(error),
                    }
                }
                Event::Saved {
                    profile,
                    source,
                    profile_saved,
                    credentials_saved,
                    reply,
                } => {
                    self.working = false;
                    if profile_saved {
                        self.profile_id = profile;
                    }
                    if credentials_saved && source == Source::Entered {
                        self.clear_secrets();
                        self.source = Source::Saved;
                        self.credential_status = Some("saved".into());
                    }
                    if let Some(error) = reply.error {
                        self.show_error(error);
                    } else {
                        self.diagnostic = None;
                        self.notice = if source == Source::File {
                            "Environment file reference saved; the input file is unchanged. Download settings applied for this session."
                        } else {
                            "Environment and credentials saved securely on this device. Download settings applied for this session."
                        }
                        .into();
                    }
                    self.send("profiles.list", json!({}), ctx);
                }
                Event::Started(reply) => {
                    self.working = false;
                    if let Some(error) = reply.error {
                        self.show_error(error);
                    } else {
                        let value = reply.result.unwrap_or(Value::Null);
                        self.run_id = value["runId"].as_str().unwrap_or("").into();
                        self.output_root = value["destination"].as_str().unwrap_or("").into();
                        self.folder_open_error = None;
                        self.journal = None;
                        self.notice.clear();
                        self.diagnostic = None;
                        self.send("run.status", json!({"id":self.run_id,"summary":true}), ctx);
                    }
                }
                Event::Reply {
                    method,
                    generation,
                    profile,
                    reply,
                } => {
                    if method == "run.status" {
                        self.poll_pending = false;
                    }
                    if matches!(method.as_str(), "credentials.forget" | "connection.disconnect") { self.working = false; }
                    if matches!(method.as_str(), "credentials.status" | "credentials.forget")
                        && (generation != self.generation || profile != self.profile_id)
                    {
                        continue;
                    }
                    if let Some(error) = reply.error {
                        self.show_error(error);
                        continue;
                    }
                    let value = reply.result.unwrap_or(Value::Null);
                    match method.as_str() {
                        "connection.disconnect" => {
                            self.connected = false;
                            self.cleanup_pending = false;
                            self.notice = "Session cleanup confirmed. You can start a new download.".into();
                            self.diagnostic = None;
                        }
                        "profiles.list" => {
                            self.profiles = serde_json::from_value(value).unwrap_or_default();
                            if !self.profiles_loaded {
                                let profile = self.profiles.iter().find(|p| p.id == self.preferences.profile_id).cloned();
                                if let Some(profile) = profile {
                                    let settings = self.settings.clone();
                                    self.select_profile(Some(profile), ctx);
                                    self.settings = settings;
                                }
                                self.profiles_loaded = true;
                            }
                        }
                        "settings.inspect" => {
                            if let Ok(settings) = serde_json::from_value(value["effective"].clone())
                            {
                                if !self.preferences_ready { self.settings = SettingsForm::new(&settings); }
                                if !self.settings_loaded { self.settings.local_decompiler_workers = settings.local_decompiler_workers.to_string(); }
                                self.settings_loaded = true;
                            }
                        }
                        "capabilities" => self.set_capabilities(value),
                        "credentials.status" => {
                            self.credential_status = value["status"].as_str().map(str::to_owned);
                        }
                        "credentials.forget" => {
                            self.clear_secrets();
                            self.username.clear();
                            self.source = Source::Entered;
                            self.credential_status = Some("unavailable".into());
                            self.notice = "Saved credentials deleted from this device. The environment is kept. Enter new credentials and Save when ready.".into();
                            self.diagnostic = None;
                            self.send("credentials.status", json!({"id":self.profile_id}), ctx);
                        }
                        "plugin.configure" => {
                            self.notice = "Collection options saved with the plugin.".into();
                            self.diagnostic = None;
                            self.send("capabilities", json!({}), ctx);
                        }
                        "run.status" => {
                            if value["runId"].as_str() == Some(&self.run_id) {
                                let cues = self.completion_sound.observe(
                                    &value, self.preferences.completion_sound,
                                );
                                self.completion_sound.queue(cues);
                                self.journal = Some(value);
                            }
                        }
                        "run.pause" | "run.resume" | "run.cancel" => {
                            self.send("run.status", json!({"id":self.run_id,"summary":true}), ctx);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    fn set_capabilities(&mut self, value: Value) {
        self.capabilities = value["categories"].as_array().cloned().unwrap_or_default();
        let present = self
            .capabilities
            .iter()
            .filter_map(|c| c["id"].as_str())
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        self.categories.retain(|id, _| present.contains(id));
        self.plugin_settings.retain(|id, _| present.contains(id));
        for category in &self.capabilities {
            let Some(id) = category["id"].as_str() else {
                continue;
            };
            let available = category["available"].as_bool() == Some(true);
            let selected = self.categories.entry(id.into()).or_insert(available && category["selected"].as_bool().unwrap_or(true));
            if !available {
                *selected = false;
            }
            if let Ok(settings) = serde_json::from_value::<Vec<herbert_contract::PluginSetting>>(
                category["settings"].clone(),
            ) {
                if let Ok(values) =
                    herbert_contract::resolve_plugin_settings(&settings, &Value::Null)
                {
                    self.plugin_settings.entry(id.into()).or_insert(values);
                }
            }
        }
        self.capabilities_loaded = true;
    }
    fn select_profile(&mut self, profile: Option<Profile>, ctx: &egui::Context) {
        self.generation += 1;
        self.clear_secrets();
        self.username.clear();
        self.credential_status = None;
        self.file_choices.clear();
        self.file_choices_path.clear();
        self.file_company_override = false;
        if let Some(profile) = profile {
            self.profile_id = profile.id;
            self.name = profile.name.unwrap_or_default();
            self.url = profile.url;
            self.company = profile.defaults.company.unwrap_or_default();
            self.mode = profile.auth_mode;
            if let Some(concurrency) = profile.defaults.concurrency {
                self.settings.concurrency = concurrency;
            }
            if let Some(file) = profile.user_file {
                self.source = Source::File;
                self.input_file = file.path;
                self.file_environment = file.environment_id;
                self.load_file_choices(ctx);
            } else {
                self.source = Source::Saved;
                self.input_file.clear();
                self.file_environment.clear();
                self.send("credentials.status", json!({"id":self.profile_id}), ctx);
            }
        } else {
            self.profile_id.clear();
            self.name.clear();
            self.url.clear();
            self.company.clear();
            self.mode = AuthMode::Session;
            self.source = Source::Entered;
            self.input_file.clear();
            self.file_environment.clear();
        }
    }
    fn apply_file_metadata(&mut self) {
        if let Some(profile) = self
            .file_choices
            .iter()
            .find(|p| p.id == self.file_environment)
        {
            self.url = profile.url.clone();
            self.mode = profile.auth_mode;
            if let Some(concurrency) = profile.defaults.concurrency {
                self.settings.concurrency = concurrency;
            }
            if !self.file_company_override {
                self.company = profile.defaults.company.clone().unwrap_or_default();
            }
            if self.name.is_empty() {
                self.name = profile.name.clone().unwrap_or_else(|| profile.id.clone());
            }
        }
    }
    fn load_file_choices(&mut self, ctx: &egui::Context) {
        self.file_choices.clear();
        self.file_choices_path.clear();
        let path = self.input_file.clone();
        let generation = self.generation;
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let result = crate::input::environment_choices(std::path::Path::new(&path)).await;
            let _ = sender.send(Event::FileChoices {
                generation,
                path,
                result,
            });
            ctx.request_repaint();
        });
    }
    fn pick_path(&mut self, picker: Picker, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.picker_pending {
            return;
        }
        self.picker_pending = true;
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_parent(frame)
            .set_title(match picker {
                Picker::Input => "Choose an environment input file",
                Picker::Destination => "Choose a download folder",
            });
        let current = match picker {
            Picker::Input => &self.input_file,
            Picker::Destination => &self.destination,
        };
        if !current.trim().is_empty() {
            let path = std::path::Path::new(current);
            if let Some(directory) = if path.is_dir() {
                Some(path)
            } else {
                path.parent()
            } {
                dialog = dialog.set_directory(directory);
            }
        }
        // Create on the UI thread for macOS, then await without blocking painting.
        let future: std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>,
        > = match picker {
            Picker::Input => Box::pin(
                dialog
                    .add_filter("JSON environment files", &["json"])
                    .pick_file(),
            ),
            Picker::Destination => Box::pin(dialog.pick_folder()),
        };
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let path = future.await.map(|handle| handle.path().to_path_buf());
            let _ = sender.send(Event::Picked { picker, path });
            ctx.request_repaint();
        });
    }
}
impl Gui {
    fn navigation(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("tabs")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Herbert").strong().size(20.0));
                    ui.add(
                        egui::Image::new(&self.brand_icon)
                            .fit_to_exact_size(egui::vec2(32.0, 32.0))
                            .alt_text("Herbert"),
                    );
                    ui.separator();
                    for (tab, label) in [
                        (Tab::Connection, "Connection"),
                        (Tab::Download, "Download"),
                        (Tab::Plugins, "Plugins"),
                        (Tab::About, "About"),
                    ] {
                        if ui
                            .add_sized(
                                [106.0, 36.0],
                                egui::Button::new(RichText::new(label).strong().size(16.0).color(
                                    ui.visuals().text_color(),
                                ))
                                .fill(if self.tab == tab {
                                    ui.visuals().widgets.active.bg_fill
                                } else { egui::Color32::TRANSPARENT })
                                .stroke(if self.tab == tab {
                                    egui::Stroke::new(1.0, ui.visuals().weak_text_color())
                                } else { egui::Stroke::NONE }),
                            )
                            .clicked()
                        {
                            self.tab = tab;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        theme_button(ui);
                    });
                });
            });
    }
}
impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.receive(&ctx);
        self.plain_text_file_help(&ctx, frame);
        self.navigation(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(20))
            .show(ui, |ui| {
                if let Some(message) = &self.preferences_error {
                    ui.label(RichText::new(message).color(ui.visuals().error_fg_color));
                }
                if self.cleanup_pending {
                    ui.label("Admission did not complete. This host still owns a session that needs cleanup.");
                    if ui.add_enabled(!self.working && !self.run_active(), egui::Button::new("Retry session cleanup")).clicked() {
                        self.working = true;
                        self.send("connection.disconnect", json!({}), &ctx);
                    }
                }
                egui::ScrollArea::vertical()
                    .id_salt(match self.tab {
                        Tab::Connection => "connection-scroll",
                        Tab::Download => "download-scroll",
                        Tab::Plugins => "plugins-scroll",
                        Tab::About => "about-scroll",
                    })
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        match self.tab {
                            Tab::Connection => self.connection_form(ui, frame),
                            Tab::Download => self.download_form(ui, frame),
                            Tab::Plugins => self.plugins_form(ui),
                            Tab::About => self.about.show(ui, &self.brand_icon),
                        }
                        ui.add_space(12.0);
                    });
            });
        // Poll only for display updates. This does not throttle collection dispatch.
        if self.run_active() && self.last_poll.elapsed() > Duration::from_secs(1) {
            self.last_poll = Instant::now();
            self.send("run.status", json!({"id":self.run_id,"summary":true}), &ctx);
        }
        if self.working || self.run_active() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        self.remember_preferences(&ctx);
    }
}
fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    #[cfg(target_os = "windows")]
    if let Some(windows) = std::env::var_os("WINDIR") {
        if let Ok(bytes) =
            std::fs::read(std::path::PathBuf::from(windows).join("Fonts/segoeui.ttf"))
        {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("system-ui".into(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "system-ui".into());
            ctx.set_fonts(fonts);
        }
    }
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            style.wrap_mode = Some(egui::TextWrapMode::Wrap);
            style.spacing.scroll = egui::style::ScrollStyle::solid();
            style.spacing.item_spacing = egui::vec2(14.0, 7.0);
            style.spacing.button_padding = egui::vec2(12.0, 7.0);
            style.spacing.interact_size.y = 32.0;
            let dark = theme == egui::Theme::Dark;
            let rgb = egui::Color32::from_rgb;
            style.visuals.override_text_color = Some(if dark {
                rgb(227, 234, 243)
            } else {
                rgb(35, 47, 63)
            });
            style.visuals.weak_text_color = Some(if dark {
                rgb(166, 182, 201)
            } else {
                rgb(91, 106, 125)
            });
            // Keep error text readable on both panel and input backgrounds.
            // The default saturated red has insufficient contrast in this theme.
            style.visuals.error_fg_color = if dark {
                rgb(255, 205, 199)
            } else {
                rgb(150, 30, 30)
            };
            style.visuals.panel_fill = if dark {
                rgb(17, 24, 34)
            } else {
                rgb(242, 245, 249)
            };
            style.visuals.faint_bg_color = if dark {
                rgb(25, 35, 49)
            } else {
                rgb(255, 255, 255)
            };
            style.visuals.extreme_bg_color = if dark {
                rgb(13, 21, 31)
            } else {
                rgb(247, 249, 252)
            };
            style.visuals.window_fill = style.visuals.faint_bg_color;
            style.visuals.window_stroke = egui::Stroke::new(
                1.0,
                if dark {
                    rgb(48, 65, 85)
                } else {
                    rgb(210, 220, 231)
                },
            );
            style.visuals.widgets.noninteractive.bg_stroke = style.visuals.window_stroke;
            style.visuals.widgets.inactive.bg_fill = if dark {
                rgb(34, 49, 68)
            } else {
                rgb(232, 239, 247)
            };
            style.visuals.widgets.inactive.weak_bg_fill = style.visuals.widgets.inactive.bg_fill;
            style.visuals.widgets.inactive.bg_stroke = style.visuals.window_stroke;
            style.visuals.widgets.hovered.bg_fill = if dark {
                rgb(46, 68, 93)
            } else {
                rgb(218, 232, 248)
            };
            style.visuals.widgets.hovered.weak_bg_fill = style.visuals.widgets.hovered.bg_fill;
            style.visuals.widgets.active.bg_fill = if dark {
                rgb(54, 82, 113)
            } else {
                rgb(204, 224, 246)
            };
            style.visuals.widgets.active.weak_bg_fill = style.visuals.widgets.active.bg_fill;
            style.visuals.selection.bg_fill = if dark {
                rgb(109, 188, 239)
            } else {
                rgb(35, 101, 179)
            };
            style.visuals.selection.stroke = egui::Stroke::new(
                1.0,
                if dark {
                    rgb(14, 32, 48)
                } else {
                    rgb(255, 255, 255)
                },
            );
            style.visuals.hyperlink_color = if dark {
                rgb(127, 198, 245)
            } else {
                rgb(25, 91, 166)
            };
            for widget in [
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
            ] {
                widget.corner_radius = egui::CornerRadius::same(6);
            }
            for kind in [egui::TextStyle::Body, egui::TextStyle::Button] {
                style
                    .text_styles
                    .insert(kind, egui::FontId::proportional(15.0));
            }
            style
                .text_styles
                .insert(egui::TextStyle::Small, egui::FontId::proportional(12.5));
            style
                .text_styles
                .insert(egui::TextStyle::Heading, egui::FontId::proportional(19.0));
        });
    }
}
fn theme_button(ui: &mut egui::Ui) {
    let dark = ui.visuals().dark_mode;
    let label = if dark {
        "Switch to light theme"
    } else {
        "Switch to dark theme"
    };
    let response = ui.add_sized([32.0, 30.0], egui::Button::new(""));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let center = response.rect.center();
    let color = ui.visuals().text_color();
    let stroke = egui::Stroke::new(1.7, color);
    if dark {
        ui.painter().circle_stroke(center, 4.5, stroke);
        for index in 0..8 {
            let angle = index as f32 * std::f32::consts::TAU / 8.0;
            let direction = egui::vec2(angle.cos(), angle.sin());
            ui.painter().line_segment(
                [center + direction * 7.0, center + direction * 10.0],
                stroke,
            );
        }
    } else {
        ui.painter().circle_filled(center, 9.0, color);
        ui.painter().circle_filled(
            center + egui::vec2(4.0, -3.0),
            8.0,
            ui.style().interact(&response).weak_bg_fill,
        );
    }
    if response.clicked() {
        ui.ctx().set_theme(if dark {
            egui::ThemePreference::Light
        } else {
            egui::ThemePreference::Dark
        });
    }
    response.on_hover_text(label);
}

#[cfg(test)]
#[path = "gui_progress_layout_tests.rs"]
mod gui_progress_layout_tests;

fn cleanup_before_login(connected: bool, cleanup_pending: bool) -> bool {
    connected || cleanup_pending
}
fn apply_connection_error(error: &Diagnostic, connected: &mut bool, cleanup_pending: &mut bool) {
    if error.context.get("connectionRetained").and_then(Value::as_bool) == Some(true) {
        *connected = false;
        *cleanup_pending = true;
    }
}
fn connection_error_message(error: &Diagnostic) -> String {
    let mut message = error.message.clone();
    if let Some(cleanup) = error.context.get("cleanup") {
        message.push_str(&format!(" Cleanup failed: {}: {}",
            cleanup["code"].as_str().unwrap_or("UNKNOWN"),
            cleanup["message"].as_str().unwrap_or("Logout is unconfirmed.")));
    }
    if error.context.get("connectionRetained").and_then(Value::as_bool) == Some(true) {
        message.push_str(" Retry session cleanup in this window. The next Download also retries cleanup before login.");
    }
    message
}
#[cfg(test)]
mod cleanup_state_tests {
    use super::*;
    #[test]
    fn failed_admission_event_preserves_cleanup_and_next_download_action() {
        let mut error = Diagnostic::new("ADMISSION_FAILED", "Admission failed.");
        error.context.insert("connectionRetained".into(), json!(true));
        error.context.insert("cleanup".into(), json!(Diagnostic::new("LOGOUT_FAILED", "Logout failed.")));
        let event = Event::Started(Reply { schema_version: 1, ok: false, result: None, error: Some(error) });
        let (mut connected, mut pending) = (false, false);
        if let Event::Started(reply) = event {
            let error = reply.error.unwrap();
            apply_connection_error(&error, &mut connected, &mut pending);
            let message = connection_error_message(&error);
            assert!(message.contains("Admission failed."));
            assert!(message.contains("LOGOUT_FAILED: Logout failed."));
        }
        assert!(!connected, "Failed admission is not a successful connection");
        assert!(pending);
        assert!(cleanup_before_login(connected, pending));
        // A failed retry has no new retention field. Preserve the owned handle.
        apply_connection_error(&Diagnostic::new("LOGOUT_FAILED", "Retry failed."), &mut connected, &mut pending);
        assert!(cleanup_before_login(connected, pending));
        assert!(!cleanup_before_login(false, false));
    }
}
