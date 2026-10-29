//! Current-user Windows DPAPI storage, independent of Credential Manager capacity.
//! Only encrypted bytes reach the OS-local directory. Existing Credential Manager
//! entries remain readable until explicitly forgotten. A damaged new record never
//! silently selects older credentials. This is user isolation, not isolation from
//! other programs running as the same Windows user.
use crate::{
    input::Credentials,
    storage::{atomic_write, sha256},
};
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io::Read,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 8] = b"HBTDP001";
const UI_FORBIDDEN: u32 = 1;
const REPARSE_POINT: u32 = 0x400;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[repr(C)]
struct Blob {
    len: u32,
    bytes: *mut u8,
}
#[link(name = "Crypt32")]
unsafe extern "system" {
    fn CryptProtectData(
        input: *const Blob,
        description: *const u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const Blob,
        description: *mut *mut u16,
        entropy: *const Blob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        output: *mut Blob,
    ) -> i32;
}
#[link(name = "Kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

fn unavailable(message: &str) -> Diagnostic {
    Diagnostic::new("SECURE_STORE_UNAVAILABLE", message)
}
fn invalid() -> Diagnostic {
    Diagnostic::new(
        "SECURE_CREDENTIAL_INVALID",
        "The saved Windows credential is damaged or belongs to another endpoint or profile. Supply credentials and save them again.",
    )
}
fn missing() -> Diagnostic {
    Diagnostic::new(
        "CREDENTIALS_REQUIRED",
        "No credentials are saved for this endpoint and profile.",
    )
}

fn blob(bytes: &[u8]) -> Result<Blob, Diagnostic> {
    // DATA_BLOB's native DWORD length is the external API boundary, not an
    // application credential-size policy.
    let len = u32::try_from(bytes.len()).map_err(|_| {
        unavailable("These credentials are too long for Windows to protect. Use a shorter password or API key, or enter the credentials for this session without saving them.")
    })?;
    Ok(Blob {
        len,
        bytes: bytes.as_ptr().cast_mut(),
    })
}

fn crypt(bytes: &[u8], entropy: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>, Diagnostic> {
    let input = blob(bytes)?;
    let context = blob(entropy)?;
    let mut output = Blob {
        len: 0,
        bytes: std::ptr::null_mut(),
    };
    // These Win32 APIs synchronously borrow the input and allocate output with
    // LocalAlloc. No UI or machine-wide protection flag is permitted.
    let success = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &context,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &context,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let result = if success != 0 && !output.bytes.is_null() {
        // The owned Rust copy and the native decrypted allocation are both
        // zeroed. The returned buffer remains zeroizing through JSON parsing.
        let native = unsafe { std::slice::from_raw_parts_mut(output.bytes, output.len as usize) };
        let value = Zeroizing::new(native.to_vec());
        native.zeroize();
        Ok(value)
    } else {
        Err(unavailable(if protect {
            "Windows could not protect the credentials for saving. Enter them for this session instead, or try saving again."
        } else {
            "Windows could not unlock the saved Epicor credentials for this profile. Enter the credentials again, or save them again from the same Windows account."
        })
        .with_context("osError", error))
    };
    if !output.bytes.is_null() {
        // Also clear a buffer returned on failure before releasing its memory.
        unsafe { std::slice::from_raw_parts_mut(output.bytes, output.len as usize) }.zeroize();
        unsafe {
            LocalFree(output.bytes.cast());
        }
    }
    result
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SecretRef<'a> {
    username: &'a str,
    password: &'a str,
    api_key: Option<&'a str>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordRef<'a> {
    format: u32,
    endpoint: &'a str,
    profile: &'a str,
    credentials: SecretRef<'a>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Record {
    format: u32,
    endpoint: String,
    profile: String,
    credentials: Credentials,
}

fn scope(endpoint: &str, profile: &str) -> Vec<u8> {
    // Length-delimited JSON distinguishes every endpoint/profile pair.
    serde_json::to_vec(&("Herbert Windows credentials v1", endpoint, profile)).unwrap()
}
fn encode(endpoint: &str, profile: &str, credentials: &Credentials) -> Result<Vec<u8>, Diagnostic> {
    let record = RecordRef {
        format: 1,
        endpoint,
        profile,
        credentials: SecretRef {
            username: &credentials.username,
            password: &credentials.password,
            api_key: credentials.api_key.as_deref(),
        },
    };
    let json = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| invalid())?);
    // An encrypted content hash supplements DPAPI integrity and structural
    // validation. It is not a separate authentication key.
    let mut plain = Zeroizing::new(Vec::with_capacity(64 + json.len()));
    plain.extend_from_slice(sha256(&json).as_bytes());
    plain.extend_from_slice(&json);
    let encrypted = crypt(&plain, &scope(endpoint, profile), true)?;
    let mut file = MAGIC.to_vec();
    file.extend_from_slice(&encrypted);
    Ok(file)
}
fn decode(bytes: &[u8], endpoint: &str, profile: &str) -> Result<Credentials, Diagnostic> {
    let encrypted = bytes
        .strip_prefix(MAGIC)
        .filter(|b| !b.is_empty())
        .ok_or_else(invalid)?;
    let plain = crypt(encrypted, &scope(endpoint, profile), false)?;
    if plain.len() < 64 || plain[..64] != *sha256(&plain[64..]).as_bytes() {
        return Err(invalid());
    }
    let record: Record = serde_json::from_slice(&plain[64..]).map_err(|_| invalid())?;
    if record.format != 1 || record.endpoint != endpoint || record.profile != profile {
        return Err(invalid());
    }
    Ok(record.credentials)
}

pub(crate) struct Store {
    root: PathBuf,
}
impl Store {
    pub(crate) fn current_user() -> Result<Self, Diagnostic> {
        // directories/dirs-sys resolves FOLDERID_LocalAppData through
        // SHGetKnownFolderPath. A user input file or portable state folder never
        // selects the secret location, and there is no environment override.
        let directories = directories::BaseDirs::new().ok_or_else(|| {
            unavailable(
                "Windows did not provide the current user's local application-data directory.",
            )
        })?;
        let root = directories
            .data_local_dir()
            .join("Herbert")
            .join("credentials-v1");
        Ok(Self { root })
    }
    fn paths(&self, endpoint: &str, profile: &str) -> (PathBuf, PathBuf) {
        let key = sha256(&scope(endpoint, profile));
        (
            self.root.join(format!("{key}.dpapi")),
            self.root.join(format!("{key}.lock")),
        )
    }
    fn lock(&self, endpoint: &str, profile: &str) -> Result<File, Diagnostic> {
        // Refuse redirection within Herbert's owned subtree. The known-folder
        // root is supplied by Windows and can legitimately be redirected by OS
        // configuration. DPAPI still binds bytes to the Windows user.
        if let Some(parent) = self.root.parent() {
            checked_directory(parent)?;
        }
        checked_directory(&self.root)?;
        let (_, lock_path) = self.paths(endpoint, profile);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(lock_path)
            .map_err(|_| unavailable("Cannot open the Windows credential lock."))?;
        checked_file(&lock)?;
        lock.lock_exclusive()
            .map_err(|_| unavailable("Cannot acquire the Windows credential lock."))?;
        Ok(lock)
    }
    fn read_locked(
        &self,
        endpoint: &str,
        profile: &str,
    ) -> Result<Option<Credentials>, Diagnostic> {
        let (path, _) = self.paths(endpoint, profile);
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(unavailable("Cannot read the saved Windows credential.")),
        };
        checked_file(&file)?;
        let mut encrypted = Vec::new();
        file.read_to_end(&mut encrypted)
            .map_err(|_| unavailable("Cannot read the complete saved Windows credential."))?;
        decode(&encrypted, endpoint, profile).map(Some)
    }
    pub(crate) fn read(
        &self,
        endpoint: &str,
        profile: &str,
        legacy: impl FnOnce() -> Result<Credentials, Diagnostic>,
    ) -> Result<Credentials, Diagnostic> {
        let _lock = self.lock(endpoint, profile)?;
        self.read_locked(endpoint, profile)?.map_or_else(legacy, Ok)
    }
    pub(crate) fn status(
        &self,
        endpoint: &str,
        profile: &str,
        legacy: impl FnOnce() -> Result<Credentials, Diagnostic>,
    ) -> Result<&'static str, Diagnostic> {
        match self.read(endpoint, profile, legacy) {
            Ok(_) => Ok("saved"),
            Err(e) if e.code == "CREDENTIALS_REQUIRED" => Ok("notSaved"),
            Err(e) => Err(e),
        }
    }
    pub(crate) fn save(
        &self,
        endpoint: &str,
        profile: &str,
        credentials: &Credentials,
    ) -> Result<(), Diagnostic> {
        let _lock = self.lock(endpoint, profile)?;
        let encrypted = encode(endpoint, profile, credentials)?;
        let (path, _) = self.paths(endpoint, profile);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_attributes() & REPARSE_POINT != 0)
        {
            return Err(unavailable(
                "The Windows credential path must not redirect to another file.",
            ));
        }
        atomic_write(&path, &encrypted)?;
        // Confirm the durable ciphertext is readable and scope-valid before the
        // caller reports saved. Never log the plaintext or OS error structures.
        let actual = self.read_locked(endpoint, profile)?.ok_or_else(missing)?;
        if actual.username != credentials.username
            || actual.password != credentials.password
            || actual.api_key != credentials.api_key
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(crate) fn forget(
        &self,
        endpoint: &str,
        profile: &str,
        legacy: impl FnOnce() -> Result<(), Diagnostic>,
    ) -> Result<(), Diagnostic> {
        let _lock = self.lock(endpoint, profile)?;
        // Remove the exact legacy credential first. If that fails, preserve the
        // active new record and report the error. Never let a forgotten new
        // record expose stale legacy credentials on the next read.
        legacy()?;
        let (path, _) = self.paths(endpoint, profile);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(unavailable("Cannot remove the saved Windows credential.")),
        }
    }
}
fn checked_directory(path: &Path) -> Result<(), Diagnostic> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(unavailable(
                "Cannot create the Windows credential directory.",
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| unavailable("Cannot inspect the Windows credential directory."))?;
    if !metadata.is_dir() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(unavailable(
            "The Windows credential directory must be a normal local directory.",
        ));
    }
    Ok(())
}
fn checked_file(file: &File) -> Result<(), Diagnostic> {
    let metadata = file
        .metadata()
        .map_err(|_| unavailable("Cannot inspect the Windows credential file."))?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err(unavailable(
            "The Windows credential file must not redirect to another file.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    const ENDPOINT: &str = "https://fixture.invalid/Epicor/";
    fn credentials(password: &str) -> Credentials {
        Credentials {
            username: "synthetic-fixture-user".into(),
            password: password.into(),
            api_key: Some("synthetic-api-key".into()),
        }
    }
    fn store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store {
            root: temp.path().join("Herbert").join("credentials-v1"),
        };
        (temp, store)
    }

    #[test]
    fn native_save_reopen_replace_and_forget_only_ciphertext() {
        let (_temp, store) = store();
        let secret = "synthetic-private-password-ᚠ";
        store.save(ENDPOINT, "p", &credentials(secret)).unwrap();
        let (path, _) = store.paths(ENDPOINT, "p");
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(secret.len())
                .any(|part| part == secret.as_bytes())
        );
        assert!(
            !bytes
                .windows(ENDPOINT.len())
                .any(|part| part == ENDPOINT.as_bytes())
        );
        let fresh = Store {
            root: store.root.clone(),
        };
        assert_eq!(
            fresh
                .read(ENDPOINT, "p", || panic!("unexpected legacy fallback"))
                .unwrap()
                .password,
            secret
        );
        fresh
            .save(ENDPOINT, "p", &credentials("replacement"))
            .unwrap();
        assert_eq!(
            fresh
                .read(ENDPOINT, "p", || Err(missing()))
                .unwrap()
                .password,
            "replacement"
        );
        fresh.forget(ENDPOINT, "p", || Ok(())).unwrap();
        assert!(!path.exists());
        assert_eq!(
            fresh.status(ENDPOINT, "p", || Err(missing())).unwrap(),
            "notSaved"
        );
    }
    #[test]
    fn native_entropy_and_record_binding_prevent_cross_endpoint_or_profile_reads() {
        let (_temp, store) = store();
        store.save(ENDPOINT, "p", &credentials("secret")).unwrap();
        let original = std::fs::read(store.paths(ENDPOINT, "p").0).unwrap();
        assert!(decode(&original, "https://different.invalid/", "p").is_err());
        assert!(decode(&original, ENDPOINT, "other-profile").is_err());
        // Even an intentionally re-encrypted body cannot evade the inner scope.
        let plain = crypt(&original[MAGIC.len()..], &scope(ENDPOINT, "p"), false).unwrap();
        let changed = crypt(&plain, &scope(ENDPOINT, "other"), true).unwrap();
        let mut record = MAGIC.to_vec();
        record.extend_from_slice(&changed);
        assert_eq!(
            decode(&record, ENDPOINT, "other").err().unwrap().code,
            "SECURE_CREDENTIAL_INVALID"
        );
    }
    #[test]
    fn corruption_never_falls_back_to_stale_legacy_credential() {
        let (_temp, store) = store();
        store.save(ENDPOINT, "p", &credentials("new")).unwrap();
        let (path, _) = store.paths(ENDPOINT, "p");
        let original = std::fs::read(&path).unwrap();
        for index in [0, MAGIC.len(), original.len() / 2, original.len() - 1] {
            let mut damaged = original.clone();
            damaged[index] ^= 0x5a;
            atomic_write(&path, &damaged).unwrap();
            assert!(
                store
                    .read(ENDPOINT, "p", || panic!(
                        "corrupt data must not select legacy"
                    ))
                    .is_err()
            );
            assert!(
                store
                    .status(ENDPOINT, "p", || panic!(
                        "corrupt status must not select legacy"
                    ))
                    .is_err()
            );
        }
        for end in [0, 7, MAGIC.len(), original.len() - 1] {
            atomic_write(&path, &original[..end]).unwrap();
            assert!(
                store
                    .read(ENDPOINT, "p", || panic!(
                        "truncated data must not select legacy"
                    ))
                    .is_err()
            );
        }
    }
    #[test]
    fn legacy_reads_are_preserved_without_automatic_migration_or_deletion() {
        let (_temp, store) = store();
        assert_eq!(
            store
                .read(ENDPOINT, "p", || Ok(credentials("legacy")))
                .unwrap()
                .password,
            "legacy"
        );
        assert!(!store.paths(ENDPOINT, "p").0.exists());
        assert_eq!(
            store
                .status(ENDPOINT, "p", || Ok(credentials("legacy")))
                .unwrap(),
            "saved"
        );
        store.save(ENDPOINT, "p", &credentials("new")).unwrap();
        assert_eq!(
            store
                .read(ENDPOINT, "p", || panic!("new record takes precedence"))
                .unwrap()
                .password,
            "new"
        );
    }
    #[test]
    fn failed_legacy_forget_preserves_active_new_record_and_other_profiles() {
        let (_temp, store) = store();
        for p in ["selected", "other"] {
            store.save(ENDPOINT, p, &credentials(p)).unwrap();
        }
        assert!(
            store
                .forget(ENDPOINT, "selected", || Err(unavailable(
                    "synthetic denial"
                )))
                .is_err()
        );
        assert_eq!(
            store
                .read(ENDPOINT, "selected", || Err(missing()))
                .unwrap()
                .password,
            "selected"
        );
        store.forget(ENDPOINT, "selected", || Ok(())).unwrap();
        assert_eq!(
            store
                .read(ENDPOINT, "other", || Err(missing()))
                .unwrap()
                .password,
            "other"
        );
        assert_eq!(
            store
                .status(ENDPOINT, "selected", || Err(missing()))
                .unwrap(),
            "notSaved"
        );
    }
    #[test]
    fn concurrent_saves_publish_whole_records_and_reads_serialize_with_forget() {
        let (_temp, store) = store();
        let gate = Arc::new(Barrier::new(3));
        let completed = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|threads| {
            for value in ["first", "second"] {
                let gate = gate.clone();
                let completed = completed.clone();
                let store = &store;
                threads.spawn(move || {
                    gate.wait();
                    store.save(ENDPOINT, "p", &credentials(value)).unwrap();
                    completed.fetch_add(1, Ordering::SeqCst);
                });
            }
            gate.wait();
        });
        assert_eq!(completed.load(Ordering::SeqCst), 2);
        let actual = store.read(ENDPOINT, "p", || Err(missing())).unwrap();
        assert!(["first", "second"].contains(&actual.password.as_str()));
        store.forget(ENDPOINT, "p", || Ok(())).unwrap();
        assert_eq!(
            store.status(ENDPOINT, "p", || Err(missing())).unwrap(),
            "notSaved"
        );
    }
    #[test]
    fn local_directory_comes_from_windows_and_is_independent_of_portable_state() {
        let store = Store::current_user().unwrap();
        let directories = directories::BaseDirs::new().unwrap();
        assert_eq!(
            store.root,
            directories
                .data_local_dir()
                .join("Herbert")
                .join("credentials-v1")
        );
        assert!(!store.root.starts_with(std::env::current_dir().unwrap()));
    }

    #[test]
    fn rejects_directory_replacing_ciphertext_or_lock() {
        let (_temp, store) = store();
        store.save(ENDPOINT, "p", &credentials("secret")).unwrap();
        let (path, lock) = store.paths(ENDPOINT, "p");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            store
                .read(ENDPOINT, "p", || panic!(
                    "unsafe file must not select legacy"
                ))
                .is_err()
        );
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        assert!(store.save(ENDPOINT, "p", &credentials("new")).is_err());
    }

    #[test]
    fn native_large_credential_has_no_credential_manager_blob_limit() {
        let (_temp, store) = store();
        let secret = "synthetic-".repeat(2048);
        store.save(ENDPOINT, "p", &credentials(&secret)).unwrap();
        assert_eq!(
            store
                .read(ENDPOINT, "p", || Err(missing()))
                .unwrap()
                .password,
            secret
        );
    }

    #[test]
    fn native_public_api_save_child_process_read_status_and_exact_forget() {
        let profile = format!("dpapi-test-{}", uuid::Uuid::new_v4());
        struct Cleanup<'a>(&'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = crate::profiles::secure_forget(ENDPOINT, self.0);
            }
        }
        let _cleanup = Cleanup(&profile);
        assert_eq!(
            crate::profiles::secure_status(ENDPOINT, &profile).unwrap(),
            "notSaved"
        );
        crate::profiles::secure_save(
            ENDPOINT,
            &profile,
            &credentials("fictional-cross-process-secret"),
        )
        .unwrap();
        assert_eq!(
            crate::profiles::secure_status(ENDPOINT, &profile).unwrap(),
            "saved"
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows_credentials::tests::native_child_reads_owned_public_api_fixture",
                "--nocapture",
            ])
            .env("HERBERT_DPAPI_TEST_PROFILE", &profile)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "The synthetic child-process credential read failed."
        );
        crate::profiles::secure_forget(ENDPOINT, &profile).unwrap();
        assert_eq!(
            crate::profiles::secure_status(ENDPOINT, &profile).unwrap(),
            "notSaved"
        );
        let (path, _) = Store::current_user().unwrap().paths(ENDPOINT, &profile);
        assert!(!path.exists());
    }

    #[test]
    fn native_child_reads_owned_public_api_fixture() {
        let Ok(profile) = std::env::var("HERBERT_DPAPI_TEST_PROFILE") else {
            return;
        };
        assert!(profile.starts_with("dpapi-test-"));
        let value = crate::profiles::secure_read(ENDPOINT, &profile).unwrap();
        assert_eq!(value.password, "fictional-cross-process-secret");
        assert_eq!(value.api_key.as_deref(), Some("synthetic-api-key"));
    }
}
