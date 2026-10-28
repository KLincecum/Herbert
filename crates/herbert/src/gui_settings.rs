//! Editable host operating settings.
//! Parse user input here. Plugin-specific values stay in the generic plugin controls.
use eframe::egui;
use herbert_contract::{Diagnostic, OperatingSettings};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct SettingsForm {
    pub concurrency: u8,
    #[serde(default = "default_server_text")]
    server_concurrency: String,
    #[serde(default = "default_worker_text")]
    pub local_decompiler_workers: String,
    connection: String,
    service: String,
    export: String,
    inactivity: String,
    interval: String,
    retries: String,
    backoff: String,
    page: String,
}
fn default_worker_text() -> String { herbert_contract::default_local_decompiler_workers().to_string() }
fn default_server_text() -> String { herbert_contract::default_server_concurrency().to_string() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_control_restores_default_and_validates_input() {
        let mut form=SettingsForm::new(&OperatingSettings::default());
        let mut old=serde_json::to_value(&form).unwrap();old.as_object_mut().unwrap().remove("local_decompiler_workers");
        assert_eq!(serde_json::from_value::<SettingsForm>(old).unwrap().value().unwrap().local_decompiler_workers,4);
        form.local_decompiler_workers="8".into();assert_eq!(form.value().unwrap().local_decompiler_workers,8);
        for invalid in ["0","-1","1.5","","4294967296"] {form.local_decompiler_workers=invalid.into();assert!(form.value().is_err());}
    }
    #[test]
    fn old_preferences_and_new_server_values_preserve_request_concurrency() {
        let mut form = SettingsForm::new(&OperatingSettings::default());
        form.concurrency = 8;
        let mut old = serde_json::to_value(&form).unwrap();
        old.as_object_mut().unwrap().remove("server_concurrency");
        let restored: SettingsForm = serde_json::from_value(old).unwrap();
        assert_eq!(restored.value().unwrap().server_concurrency, 4);
        form.server_concurrency = "6".into();
        assert_eq!(form.value().unwrap().server_concurrency, 6);
        assert_eq!(form.value().unwrap().concurrency, 8);
        form.server_concurrency = "0".into();
        assert!(form.value().is_err());
    }
}
fn optional(text: &str) -> Result<Option<u64>, Diagnostic> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    text.trim().parse().map(Some).map_err(|_| {
        Diagnostic::new(
            "INVALID_SETTING",
            "Enter a whole number, or leave the optional value blank.",
        )
    })
}
fn small(text: &str) -> Result<Option<u32>, Diagnostic> {
    optional(text)?
        .map(|n| {
            u32::try_from(n).map_err(|_| {
                Diagnostic::new(
                    "INVALID_SETTING",
                    "The setting exceeds its u32 representation.",
                )
            })
        })
        .transpose()
}
impl SettingsForm {
    pub fn new(settings: &OperatingSettings) -> Self {
        let text = |value: Option<u64>| value.map(|n| n.to_string()).unwrap_or_default();
        Self {
            concurrency: settings.concurrency,
            server_concurrency: settings.server_concurrency.to_string(),
            local_decompiler_workers: settings.local_decompiler_workers.to_string(),
            connection: text(settings.connection_timeout_ms),
            service: text(settings.service_timeout_ms),
            export: text(settings.export_timeout_ms),
            inactivity: text(settings.inactivity_timeout_ms),
            interval: text(settings.minimum_request_interval_ms),
            retries: settings.retry_attempts.to_string(),
            backoff: text(settings.retry_backoff_ms),
            page: settings
                .page_size
                .map(|n| n.to_string())
                .unwrap_or_default(),
        }
    }
    pub fn value(&self) -> Result<OperatingSettings, Diagnostic> {
        let value = OperatingSettings {
            concurrency: self.concurrency,
            server_concurrency: self.server_concurrency.trim().parse().map_err(|_| {
                Diagnostic::new("INVALID_SERVER_CONCURRENCY", "Enter a positive whole number for server concurrency.")
            })?,
            local_decompiler_workers: self.local_decompiler_workers.trim().parse().map_err(|_| Diagnostic::new("INVALID_DECOMPILER_WORKERS", "Enter a positive whole number for local decompiler workers."))?,
            connection_timeout_ms: optional(&self.connection)?,
            service_timeout_ms: optional(&self.service)?,
            export_timeout_ms: optional(&self.export)?,
            inactivity_timeout_ms: optional(&self.inactivity)?,
            minimum_request_interval_ms: optional(&self.interval)?,
            retry_attempts: self.retries.trim().parse().map_err(|_| {
                Diagnostic::new("INVALID_SETTING", "Retry attempts must be a whole number.")
            })?,
            retry_backoff_ms: optional(&self.backoff)?,
            page_size: small(&self.page)?,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Simultaneous plugins");
            for count in [2, 4, 6, 8] {
                ui.selectable_value(&mut self.concurrency, count, count.to_string())
                    .on_hover_text("Each plugin sends one request at a time. Its next page can download while received files are processed.");
            }
            ui.separator();
            ui.label("Server concurrency");
            ui.add(egui::TextEdit::singleline(&mut self.server_concurrency).desired_width(50.0))
                .on_hover_text("Parallel work inside one server collection. Only one server collection runs at a time across plugins.");
            ui.separator();
            let label=ui.label("Local decompiler workers");
            ui.add(egui::TextEdit::singleline(&mut self.local_decompiler_workers).desired_width(60.0)).labelled_by(label.id)
                .on_hover_text("One shared local worker budget per run across all plugins. Default: 4. Changes apply to new runs; active raw jobs keep their current batch budget.");
        });
        ui.add_space(4.0);
        // Labels sit above inputs so long names never compete with a grid's
        // previous-frame column measurements or the reserved scrollbar width.
        let column_count = if ui.available_width() >= 960.0 { 4 } else if ui.available_width() >= 640.0 { 2 } else { 1 };
        let mut fields = [
            ("Connection timeout (ms)", &mut self.connection, "No timeout"),
            ("Service timeout (ms)", &mut self.service, "No timeout"),
            ("Export timeout (ms)", &mut self.export, "No timeout"),
            ("Transfer inactivity (ms)", &mut self.inactivity, "No timeout"),
            ("Request interval (ms)", &mut self.interval, "0 — no added delay"),
            ("Retry attempts", &mut self.retries, "0"),
            ("Retry backoff (ms)", &mut self.backoff, "0 — no added delay"),
            ("Fallback page size", &mut self.page, "Plugin / server default"),
        ];
        for row in fields.chunks_mut(column_count) {
            ui.columns(column_count, |columns| {
                for ((label, value, empty), column) in row.iter_mut().zip(columns) {
                    let response = column.label(*label);
                    column
                        .add(egui::TextEdit::singleline(*value).hint_text(*empty).desired_width(f32::INFINITY))
                        .labelled_by(response.id);
                }
            });
            ui.add_space(4.0);
        }
        if ui.button("Restore download defaults").clicked() {
            *self = Self::new(&OperatingSettings::default());
        }
    }
}
