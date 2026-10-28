//! Environment metadata and explicit operating-system credential storage.
//! Profiles contain references. Secret values belong in the selected credential facility.
use crate::{
    input::{Credentials, Profile, validate_profile},
    storage::{atomic_write, sha256},
};
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProfileFile {
    schema_version: u32,
    profiles: BTreeMap<String, Profile>,
}
pub struct Profiles {
    path: PathBuf,
}
impl Profiles {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("profiles.json"),
        }
    }
    fn load(&self) -> Result<ProfileFile, Diagnostic> {
        match std::fs::read(&self.path) {
            Ok(b) => {
                let file: ProfileFile = serde_json::from_slice(&b).map_err(|_| {
                    Diagnostic::new(
                        "PROFILE_STORAGE_INVALID",
                        "Profile storage is invalid; preserve it and repair its structure.",
                    )
                })?;
                if file.schema_version != 1 {
                    return Err(Diagnostic::new(
                        "UNKNOWN_SCHEMA",
                        "Profile storage uses an unsupported schema.",
                    ));
                }
                for p in file.profiles.values() {
                    validate_profile(p)?;
                }
                Ok(file)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProfileFile {
                schema_version: 1,
                ..Default::default()
            }),
            Err(_) => Err(Diagnostic::new(
                "PROFILE_STORAGE_UNAVAILABLE",
                "Cannot read saved profiles.",
            )),
        }
    }
    fn change(
        &self,
        change: impl FnOnce(&mut ProfileFile) -> Result<(), Diagnostic>,
    ) -> Result<(), Diagnostic> {
        // All processes sharing this profile folder serialize the complete
        // read/change/commit operation. Atomic replacement alone loses updates.
        let directory = self.path.parent().ok_or_else(|| {
            Diagnostic::new(
                "PROFILE_STORAGE_UNAVAILABLE",
                "The profile directory is unavailable.",
            )
        })?;
        if std::fs::symlink_metadata(directory).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Diagnostic::new(
                "UNSAFE_PATH",
                "The profile directory cannot be a symbolic link.",
            ));
        }
        std::fs::create_dir_all(directory).map_err(|_| {
            Diagnostic::new(
                "PROFILE_STORAGE_UNAVAILABLE",
                "Cannot create the profile directory.",
            )
        })?;
        let lock_path = self.path.with_extension("lock");
        if std::fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Diagnostic::new(
                "UNSAFE_PATH",
                "The profile lock cannot be a symbolic link.",
            ));
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|_| {
                Diagnostic::new(
                    "PROFILE_STORAGE_UNAVAILABLE",
                    "Cannot open the profile lock.",
                )
            })?;
        lock.lock_exclusive().map_err(|_| {
            Diagnostic::new(
                "PROFILE_STORAGE_UNAVAILABLE",
                "Cannot acquire the profile lock.",
            )
        })?;
        let mut file = self.load()?;
        change(&mut file)?;
        atomic_write(&self.path, &serde_json::to_vec_pretty(&file).unwrap())
    }
    pub fn edit(&self, id: &str, edit: impl FnOnce(&mut Profile)) -> Result<(), Diagnostic> {
        self.change(|file| {
            let profile = file.profiles.get_mut(id).ok_or_else(|| {
                Diagnostic::new(
                    "PROFILE_NOT_FOUND",
                    "The exact selected profile does not exist.",
                )
            })?;
            edit(profile);
            validate_profile(profile)
        })
    }
    pub fn list(&self) -> Result<Vec<Profile>, Diagnostic> {
        Ok(self.load()?.profiles.into_values().collect())
    }
    pub fn get(&self, id: &str) -> Result<Profile, Diagnostic> {
        self.load()?.profiles.remove(id).ok_or_else(|| {
            Diagnostic::new(
                "PROFILE_NOT_FOUND",
                "The exact selected profile does not exist.",
            )
        })
    }
    pub fn upsert(&self, mut profile: Profile) -> Result<(), Diagnostic> {
        validate_profile(&profile)?;
        profile.url = crate::input::endpoint(&profile.url)?.to_string();
        self.change(|file| {
            file.profiles.insert(profile.id.clone(), profile);
            Ok(())
        })
    }
    pub fn remove(&self, id: &str) -> Result<(), Diagnostic> {
        self.change(|file| {
            if file.profiles.remove(id).is_none() {
                return Err(Diagnostic::new(
                    "PROFILE_NOT_FOUND",
                    "The exact selected profile does not exist.",
                ));
            }
            Ok(())
        })
    }
}
fn entry(endpoint: &str, profile: &str) -> Result<keyring::Entry, Diagnostic> {
    let account = sha256(format!("{endpoint}\0{profile}").as_bytes());
    keyring::Entry::new("Herbert", &account).map_err(|_| {
        Diagnostic::new(
            "SECURE_STORE_UNAVAILABLE",
            "The operating system credential facility is unavailable.",
        )
    })
}
fn store_error(error: keyring::Error) -> Diagnostic {
    let code = if matches!(error, keyring::Error::NoEntry) {
        "CREDENTIALS_REQUIRED"
    } else {
        "SECURE_STORE_UNAVAILABLE"
    };
    let mut message = "Secure credentials are unavailable, locked or denied. Supply credentials explicitly for this connection or use your own input file.".to_string();
    #[cfg(target_os = "windows")]
    if let keyring::Error::PlatformFailure(cause) | keyring::Error::NoStorageAccess(cause) = &error
    {
        // This type contains a Win32 numeric code only. Do not format an
        // arbitrary backend error. Some keyring variants contain credential data.
        if let Some(native) = cause.downcast_ref::<keyring::windows::Error>() {
            message.push_str(&format!(" Windows credential error: {}.", native.0));
        }
    }
    if let keyring::Error::TooLong(attribute, limit) = &error {
        message.push_str(&format!(
            " The native facility limits {attribute} to {limit}."
        ));
    }
    Diagnostic::new(code, message)
}
pub fn secure_status(endpoint: &str, profile: &str) -> Result<&'static str, Diagnostic> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_credentials::Store::current_user()?
            .status(endpoint, profile, || legacy_read(endpoint, profile))
    }
    #[cfg(not(target_os = "windows"))]
    {
        match entry(endpoint, profile)?.get_password() {
            Ok(value) => {
                let _ = Zeroizing::new(value);
                Ok("saved")
            }
            Err(keyring::Error::NoEntry) => Ok("notSaved"),
            Err(e) => Err(store_error(e)),
        }
    }
}
pub fn secure_save(
    endpoint: &str,
    profile: &str,
    credentials: &Credentials,
) -> Result<(), Diagnostic> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_credentials::Store::current_user()?.save(endpoint, profile, credentials)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let value = Zeroizing::new(serde_json::json!({"username":credentials.username,"password":credentials.password,"apiKey":credentials.api_key}).to_string());
        entry(endpoint, profile)?
            .set_password(value.as_str())
            .map_err(store_error)
    }
}
pub fn secure_read(endpoint: &str, profile: &str) -> Result<Credentials, Diagnostic> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_credentials::Store::current_user()?
            .read(endpoint, profile, || legacy_read(endpoint, profile))
    }
    #[cfg(not(target_os = "windows"))]
    {
        legacy_read(endpoint, profile)
    }
}
fn legacy_read(endpoint: &str, profile: &str) -> Result<Credentials, Diagnostic> {
    let value = Zeroizing::new(
        entry(endpoint, profile)?
            .get_password()
            .map_err(store_error)?,
    );
    serde_json::from_str(&value).map_err(|_| {
        Diagnostic::new(
            "SECURE_CREDENTIAL_INVALID",
            "The saved secure credential does not match Herbert's credential contract.",
        )
    })
}
pub fn secure_forget(endpoint: &str, profile: &str) -> Result<(), Diagnostic> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_credentials::Store::current_user()?
            .forget(endpoint, profile, || legacy_forget(endpoint, profile))
    }
    #[cfg(not(target_os = "windows"))]
    {
        legacy_forget(endpoint, profile)
    }
}
fn legacy_forget(endpoint: &str, profile: &str) -> Result<(), Diagnostic> {
    match entry(endpoint, profile)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(store_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_profile_uses_normalized_instance_url() {
        let root = tempfile::tempdir().unwrap();
        let profiles = Profiles::new(root.path());
        profiles.upsert(Profile { id: "lab".into(), name: Some("Lab".into()),
            url: "https://EXAMPLE.test/MyInstance/APPS/erp/Home?company=C#screen".into(),
            auth_mode: Default::default(), defaults: Default::default(), user_file: None }).unwrap();
        assert_eq!(profiles.get("lab").unwrap().url, "https://example.test/MyInstance/");
    }

    #[test]
    fn concurrent_profile_updates_keep_both_profiles_and_unrelated_fields() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("new").join("state");
        let initial = Profile {
            id: "first".into(),
            name: None,
            url: "https://fixture.invalid/".into(),
            auth_mode: Default::default(),
            defaults: Default::default(),
            user_file: None,
        };
        // A first launch must create its directory before opening the lock.
        Profiles::new(&state).upsert(initial.clone()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        std::thread::scope(|scope| {
            let gate = barrier.clone();
            let path = state.as_path();
            scope.spawn(move || {
                gate.wait();
                Profiles::new(path)
                    .edit("first", |p| p.name = Some("renamed".into()))
                    .unwrap();
            });
            let gate = barrier.clone();
            scope.spawn(move || {
                gate.wait();
                let mut second = initial;
                second.id = "second".into();
                Profiles::new(path).upsert(second).unwrap();
            });
            barrier.wait();
        });
        let profiles = Profiles::new(&state);
        assert_eq!(profiles.list().unwrap().len(), 2);
        assert_eq!(
            profiles.get("first").unwrap().name.as_deref(),
            Some("renamed")
        );
        assert_eq!(
            profiles.get("second").unwrap().url,
            "https://fixture.invalid/"
        );
    }
}
