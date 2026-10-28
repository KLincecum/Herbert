//! Host presentation preferences only. Plugin selection and values live with plugins.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Preferences {
    schema_version: u32,
    pub profile_id: String,
    pub settings: SettingsForm,
    destination: String,
    output_mode: Option<bool>,
    #[serde(default = "crate::application::default_temporary_access")]
    temporary_access: bool,
    dark: bool,
    #[serde(default = "sound_enabled")]
    pub completion_sound: bool,
}
fn sound_enabled() -> bool { true }
impl Default for Preferences {
    fn default() -> Self {
        Self { schema_version: 1, profile_id: String::new(), settings: SettingsForm::new(&Default::default()), destination: String::new(), output_mode: Some(true), temporary_access: crate::application::default_temporary_access(), dark: true, completion_sound: true }
    }
}
impl Gui {
    pub(super) fn load_preferences(&mut self, ctx: &egui::Context) {
        match crate::collection_preferences::read(&self.host.state_root) {
            Ok(p) => self.collection_method = p.collection_method,
            Err(e) => self.show_error(e),
        }
        let path = self.host.state_root.join("gui-preferences.json");
        let result = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Preferences>(&bytes).map(Some).map_err(|_| "Saved app settings could not be read. The original file has been preserved."),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err("Saved app settings could not be opened. Changes cannot be saved yet."),
        };
        match result {
            Ok(Some(p)) if p.schema_version == 1 => {
                self.settings = p.settings.clone();
                self.destination = p.destination.clone();
                self.output_mode = p.output_mode.or(Some(true));
                self.temporary_access = p.temporary_access;
                ctx.set_theme(if p.dark { egui::ThemePreference::Dark } else { egui::ThemePreference::Light });
                self.preferences = p;
                self.preferences_ready = true;
            }
            Ok(None) => self.preferences_ready = true,
            Ok(Some(_)) => self.preferences_error = Some("Saved app settings use an unsupported version. The original file has been preserved.".into()),
            Err(message) => self.preferences_error = Some(message.into()),
        }
        match crate::mutation_preferences::take_notice(&self.host.state_root) {
            Ok(Some(message)) => self.notice = message,
            Ok(None) => {},
            Err(error) => self.show_error(error),
        }
    }
    pub(super) fn remember_preferences(&mut self, ctx: &egui::Context) {
        if !self.preferences_ready || !self.profiles_loaded || !self.settings_loaded { return; }
        let preferences = Preferences {
            schema_version: 1, profile_id: self.profile_id.clone(), settings: self.settings.clone(),
            destination: self.destination.clone(), output_mode: self.output_mode,
            temporary_access: self.temporary_access, dark: ctx.theme() == egui::Theme::Dark,
            completion_sound: self.preferences.completion_sound,
        };
        let bytes = serde_json::to_vec_pretty(&preferences).expect("Host preferences contain serializable fields");
        if bytes == self.preferences_bytes { return; }
        match crate::storage::atomic_write(&self.host.state_root.join("gui-preferences.json"), &bytes) {
            Ok(()) => { self.preferences_bytes = bytes; self.preferences = preferences; self.preferences_error = None; }
            Err(_) => self.preferences_error = Some("App settings could not be saved. Your current choices still apply to this session.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn temporary_access_defaults_off_without_overwriting_explicit_choice() {
        let defaults = Preferences::default();
        assert!(!defaults.temporary_access);
        let mut value = serde_json::to_value(defaults).unwrap();
        value.as_object_mut().unwrap().remove("temporaryAccess");
        let missing: Preferences = serde_json::from_value(value.clone()).unwrap();
        assert!(!missing.temporary_access);
        value["temporaryAccess"] = serde_json::json!(false);
        let opted_out: Preferences = serde_json::from_value(value).unwrap();
        assert!(!opted_out.temporary_access);
        let restored: Preferences = serde_json::from_value(serde_json::to_value(opted_out).unwrap()).unwrap();
        assert!(!restored.temporary_access);
    }
    #[test]
    fn host_preferences_round_trip_without_plugin_or_credential_fields() {
        let mut p = Preferences::default();
        p.profile_id = "lab".into(); p.destination = "chosen folder".into(); p.output_mode = Some(false); p.temporary_access = true;
        p.settings.concurrency = 6;
        let value = serde_json::to_value(&p).unwrap();
        let restored: Preferences = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(restored.profile_id, "lab"); assert_eq!(restored.destination,"chosen folder"); assert_eq!(restored.settings.concurrency,6); assert_eq!(restored.output_mode,Some(false)); assert!(restored.temporary_access);
        let keys = value.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(keys, vec!["completionSound","dark","destination","outputMode","profileId","schemaVersion","settings","temporaryAccess"]);
    }
    #[test]
    fn completion_sound_preserves_saved_mute_and_loads_older_preferences() {
        let mut value = serde_json::to_value(Preferences::default()).unwrap();
        value.as_object_mut().unwrap().remove("completionSound");
        assert!(serde_json::from_value::<Preferences>(value.clone()).unwrap().completion_sound);
        value["completionSound"] = serde_json::json!(false);
        let muted: Preferences = serde_json::from_value(value).unwrap();
        assert!(!serde_json::from_value::<Preferences>(serde_json::to_value(muted).unwrap()).unwrap().completion_sound);
    }
}
