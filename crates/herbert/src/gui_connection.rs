//! Connection screen: certificate option, Test connection and the company field.
//! Test connection reuses connection.test, which always signs out afterwards.
use super::*;

pub(super) const CERTIFICATE_WARNING: &str = "Herbert will accept this server's certificate even though Windows does not trust it. Turn this on only for a server you control, such as a test server or one that uses your organization's own certificate authority. Herbert still checks the server name and expiry date. This setting applies only to this profile.";
pub(super) const EMPTY_COMPANY: &str = "Choose a company for this environment. Use Test connection to list the companies your account can use.";
const RELAXED: &str = "Certificate checks are relaxed for this profile";
pub(super) const UNENCRYPTED: &str = "This connection is not encrypted. The URL uses http, so no certificate was checked and data travels in plain text.";

pub(super) fn warning(ui: &mut egui::Ui, text: &str) {
    let color = ui.visuals().warn_fg_color;
    ui.label(RichText::new(text).small().color(color));
}

/// Filled primary action, matching the Download preview button.
pub(super) fn primary_button(ui: &egui::Ui, text: &str, min: egui::Vec2) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).strong().color(ui.visuals().selection.stroke.color))
        .fill(ui.visuals().selection.bg_fill).min_size(min)
}

/// Short pill shown before an environment label when the profile relaxes trust.
pub(super) fn trust_pill(ui: &mut egui::Ui, relaxed: bool, detail: Option<&Value>) {
    if !relaxed { return; }
    let fill = ui.visuals().warn_fg_color.linear_multiply(0.25);
    let text_color = ui.visuals().strong_text_color();
    let response = egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(10, 4)).corner_radius(5)
        .show(ui, |ui| ui.label(RichText::new("Self-signed certificate").strong().color(text_color))).response;
    response.on_hover_text(trust_hover(detail));
}

fn trust_hover(detail: Option<&Value>) -> String {
    let mut text = format!("{RELAXED}.");
    if let Some(trust) = detail {
        if trust["mode"] == "verified" { text.push_str(" Option on; the last test verified the certificate normally."); }
        for (label, key) in [("Subject", "subject"), ("Issuer", "issuer"), ("SHA-256", "certificateSha256")] {
            if let Some(value) = trust[key].as_str() { text.push_str(&format!("\n{label}: {value}")); }
        }
        text.push_str("\nThe fingerprint can change whenever the server renews its certificate.");
    }
    text
}

/// The first 12 hex characters, with the full value on hover and a copy action.
fn fingerprint(ui: &mut egui::Ui, sha256: &str) {
    let short: String = sha256.chars().take(12).collect();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Certificate fingerprint {short}…")).small()).on_hover_text(sha256);
        if ui.small_button("Copy").on_hover_text("Copy the full SHA-256 fingerprint").clicked() { ui.ctx().copy_text(sha256.to_owned()); }
    });
}

impl Gui {
    pub(super) fn certificate_option(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.allow_untrusted_certificate, "Allow self-signed certificate")
            .on_hover_text("Also accepts a certificate from a private certificate authority that Windows does not trust.");
        if self.allow_untrusted_certificate { warning(ui, CERTIFICATE_WARNING); }
    }
    /// The company list depends on endpoint, credentials and certificate option, not the chosen company.
    pub(super) fn company_list_key(&self) -> Option<String> {
        let mut arguments = self.connection_arguments(&self.settings.value().ok()?).ok()?;
        arguments.as_object_mut()?.remove("company");
        Some(super::gui_server_changes::connection_key(&arguments, false))
    }
    fn company_list(&self) -> Option<&Value> {
        let key = self.company_list_key()?;
        self.company_choices.as_ref().filter(|(saved, _)| *saved == key).map(|(_, result)| result)
    }
    pub(super) fn test_connection_button(&mut self, ui: &mut egui::Ui) {
        let missing = if self.url.trim().is_empty() { Some("Enter the Epicor instance URL first.") }
            else if self.source == Source::Entered && self.username.trim().is_empty() { Some("Enter the username first.") }
            else if self.connected || self.cleanup_pending { Some("Finish or disconnect the current connection first.") }
            else { None };
        let enabled = self.editable() && !self.testing_connection && missing.is_none();
        let response = ui.add_enabled(enabled, egui::Button::new("Test connection").min_size(egui::vec2(120.0, 34.0)))
            .on_hover_text("Signs in with these settings, checks the account and lists the companies you can use, then signs out. It collects nothing and changes nothing on the server.")
            .on_disabled_hover_text(missing.unwrap_or("A test is already running."));
        if response.clicked() { self.test_connection(ui.ctx()); }
        if self.testing_connection { ui.spinner(); ui.label("Testing connection…"); }
    }
    fn test_connection(&mut self, ctx: &egui::Context) {
        let input = (|| Ok::<_, Diagnostic>((self.account_key()?, self.company_list_key(), self.connection_arguments(&self.settings.value()?)?)))();
        let (key, list_key, arguments) = match input { Ok(input) => input, Err(error) => { self.show_error(error); return; } };
        self.testing_connection = true;
        self.account_readiness = None;
        let host = self.host.clone(); let sender = self.sender.clone(); let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let reply = host.execute(Command { schema_version: 1, method: "connection.test".into(), arguments }).await;
            if let (Some(list_key), Some(result)) = (list_key, reply.result.clone()) {
                let _ = sender.send(Event::CompanyChoices { key: list_key, result });
            }
            let _ = sender.send(Event::AccountReadiness { key, reply, testing: true });
            ctx.request_repaint();
        });
    }
    /// Readiness, certificate state and company list from the last matching test.
    pub(super) fn connection_test_result(&mut self, ui: &mut egui::Ui) {
        self.account_readiness_panel(ui);
        if let Some(trust) = self.company_list().map(|result| result["transportTrust"].clone()) {
            if trust["mode"] == "relaxedUntrustedIssuer" { ui.label(RichText::new(RELAXED).strong()); }
            if trust["mode"] == "unencrypted" { warning(ui, UNENCRYPTED); }
            if let Some(sha256) = trust["certificateSha256"].as_str().filter(|_| trust["mode"] == "relaxedUntrustedIssuer") { fingerprint(ui, sha256); }
        }
    }
    /// Company field: a text field until a test returns a list, then a dropdown.
    pub(super) fn company_field(&mut self, ui: &mut egui::Ui) {
        let label = ui.label("Company");
        let list = self.company_list().cloned();
        let companies = list.as_ref().and_then(|result| result["availableCompanies"].as_array().cloned()).unwrap_or_default();
        let unavailable = list.as_ref().is_some_and(|result| result["companySource"] == "unavailable");
        if list.is_some() && !unavailable {
            let entry = |row: &Value| format!("{} ({})", row["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(row["id"].as_str().unwrap_or("")), row["id"].as_str().unwrap_or(""));
            let selected = companies.iter().find(|row| row["id"] == self.company.as_str()).map(entry)
                .unwrap_or_else(|| if self.company.is_empty() { "Choose a company".into() } else { self.company.clone() });
            egui::ComboBox::from_id_salt("company").width(ui.available_width()).selected_text(selected).show_ui(ui, |ui| {
                for row in &companies {
                    let id = row["id"].as_str().unwrap_or("").to_owned();
                    ui.selectable_value(&mut self.company, id, entry(row));
                }
            }).response.labelled_by(label.id);
            if list.as_ref().is_some_and(|result| result["companySource"] == "companyList") {
                ui.label(RichText::new("All companies are listed. Access to each company is confirmed when you connect.").small().weak());
            }
            if !self.company.is_empty() && !companies.iter().any(|row| row["id"] == self.company.as_str()) {
                warning(ui, &format!("Saved company {} is not in the list of companies this account can use. Choose one from the list.", self.company));
            }
        } else {
            ui.add(egui::TextEdit::singleline(&mut self.company).desired_width(f32::INFINITY)
                .hint_text(RichText::new("Use Test connection to list your companies").weak())).labelled_by(label.id);
            if unavailable {
                let reason = list.as_ref().and_then(|result| result["companyListDiagnostic"]["message"].as_str()).unwrap_or("The list could not be read.");
                ui.label(RichText::new(format!("Company list unavailable: {reason} Enter the company ID manually.")).small().weak());
            }
        }
        if self.company.trim().is_empty() { warning(ui, EMPTY_COMPANY); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn render(app: &mut Gui, width: f32, height: f32) -> (String, egui::Vec2, egui::Rect) {
        let ctx = app_context();
        let mut text = String::new(); let mut geometry = None;
        for _ in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))), ..Default::default() }, |ui| {
                egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(20)).show(ui, |ui| {
                    let scroll = egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        app.connection_card(ui);
                    });
                    geometry = Some((scroll.content_size, scroll.inner_rect));
                });
            });
            output.textures_delta.clear();
            text = output.shapes.into_iter().filter_map(|shape| if let egui::Shape::Text(text) = shape.shape { Some(text.galley.text().to_owned()) } else { None }).collect::<Vec<_>>().join(" | ");
        }
        let (content, viewport) = geometry.unwrap();
        (text, content, viewport)
    }
    fn app_context() -> egui::Context { let ctx = egui::Context::default(); configure_style(&ctx); ctx }
    fn fixture() -> (tokio::runtime::Runtime, Gui) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ctx = app_context();
        let mut app = super::super::gui_progress_layout_tests::app(&ctx, &runtime, false);
        app.run_id.clear(); app.journal = None; app.profiles_loaded = true; app.settings_loaded = true;
        app.source = Source::Entered; app.url = "https://erp.example.invalid/Kinetic".into(); app.username = "manager".into(); app.password = "fixture".into();
        (runtime, app)
    }
    fn tested(app: &mut Gui, result: Value) {
        let key = app.company_list_key().unwrap();
        app.company_choices = Some((key, result));
    }

    #[test]
    fn gui_profile_shows_option_warning_and_relaxed_badge() {
        let (_runtime, mut app) = fixture();
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert!(text.contains("Allow self-signed certificate") && !text.contains("Herbert will accept this server"), "{text}");
        app.allow_untrusted_certificate = true;
        tested(&mut app, json!({"availableCompanies":[],"companySource":"userAccess","transportTrust":{"mode":"relaxedUntrustedIssuer","certificateSha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}));
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert!(text.contains("Herbert will accept this server's certificate"), "{text}");
        assert!(text.contains(RELAXED), "{text}");
        assert!(text.contains("Certificate fingerprint 0123456789ab…"), "{text}");
    }

    #[test]
    fn gui_company_dropdown_keeps_saved_value_and_allows_free_entry_only_when_unavailable() {
        let (_runtime, mut app) = fixture();
        app.company = "OLD".into();
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert!(!text.contains("Choose a company"), "text field before a test: {text}");
        tested(&mut app, json!({"availableCompanies":[{"id":"EPIC","name":"Epic Corp"}],"companySource":"userAccess","selectedCompany":{"id":"OLD","inList":false}}));
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert_eq!(app.company, "OLD", "the saved value is kept");
        assert!(text.contains("Saved company OLD is not in the list"), "{text}");
        tested(&mut app, json!({"availableCompanies":[],"companySource":"unavailable","companyListDiagnostic":{"code":"TRANSPORT_FAILED","message":"The list could not be read.","retryable":false}}));
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert!(text.contains("Company list unavailable") && text.contains("Enter the company ID manually"), "{text}");
    }

    #[test]
    fn company_list_cache_clears_when_connection_identity_changes() {
        let (_runtime, mut app) = fixture();
        tested(&mut app, json!({"availableCompanies":[{"id":"EPIC","name":"Epic"}],"companySource":"userAccess"}));
        assert!(app.company_list().is_some());
        app.company = "EPIC".into();
        assert!(app.company_list().is_some(), "choosing a company does not invalidate the list");
        app.allow_untrusted_certificate = true;
        assert!(app.company_list().is_none(), "the certificate option is part of the connection identity");
        app.allow_untrusted_certificate = false;
        app.url = "https://other.example.invalid/Kinetic".into();
        assert!(app.company_list().is_none(), "a new endpoint needs a new test");
    }

    #[test]
    fn empty_company_blocks_gui_start_with_guidance() {
        let (_runtime, mut app) = fixture();
        let (text, ..) = render(&mut app, 1180.0, 820.0);
        assert!(text.contains(EMPTY_COMPANY), "{text}");
        assert!(app.company_missing());
        app.company = "EPIC".into();
        assert!(!app.company_missing());
    }

    #[test]
    fn connection_card_fits_at_720_and_1180_with_certificate_test_and_company_states() {
        for width in [720.0, 1180.0] {
            for (allow, testing, populated) in [(false, false, false), (true, false, false), (true, true, false), (false, false, true)] {
                let (_runtime, mut app) = fixture();
                app.allow_untrusted_certificate = allow; app.testing_connection = testing;
                if populated { tested(&mut app, json!({"availableCompanies":(0..8).map(|n|json!({"id":format!("COMPANY{n}"),"name":format!("Company {n}")})).collect::<Vec<_>>(),"companySource":"userAccess"})); }
                let (_, content, viewport) = render(&mut app, width, 820.0);
                assert!(content.x <= viewport.width() + 1.0, "horizontal overflow at {width} allow={allow} testing={testing} populated={populated}: {content:?} in {viewport:?}");
                // Off-screen widgets are not painted, so read the text in a tall viewport.
                let (text, ..) = render(&mut app, width, 2400.0);
                if testing { assert!(text.contains("Testing connection…") && text.contains("Verifying…"), "{text}"); }
                if populated { assert!(text.contains("Company 0 (COMPANY0)") || text.contains("Choose a company"), "{text}"); }
            }
        }
    }
}
