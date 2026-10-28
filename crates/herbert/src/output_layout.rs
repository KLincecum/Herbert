//! Human-facing names are independent from immutable internal object identities.
use crate::storage::{Storage, atomic_write, collision_key, identity_key, sha256};
use herbert_contract::{Diagnostic, Identity, Scope};
use serde::{Deserialize, Serialize};
use std::fs;

/// Escape only characters Windows cannot represent, without collapsing distinct names.
pub(crate) fn component(value: &str) -> String {
    let mut name = String::new();
    let length = value.chars().count();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_control()
            || "<>:\"/\\|?*%".contains(ch)
            || (index == 0 && ch == '.')
            || ((ch == '.' || ch == ' ') && index + 1 == length)
        {
            for byte in ch.to_string().bytes() {
                name.push_str(&format!("%{byte:02X}"));
            }
        } else {
            name.push(ch);
        }
    }
    if name.is_empty() {
        name.push_str("Unnamed");
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        name.insert_str(0, "%5F");
    }
    bounded(name)
}

pub(crate) fn bounded(mut name: String) -> String {
    // NTFS allows 255 UTF-16 units per component. Common Linux filesystems allow
    // 255 bytes. Shorten display names only at that physical boundary. Identity
    // remains complete in the naming registry and the full digest prevents loss.
    if name.len() > 255 || name.encode_utf16().count() > 255 {
        let digest = sha256(name.as_bytes());
        let suffix = name
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            // Preserve any extension that fits beside the complete digest.
            // An extension larger than that cannot fit in one portable component.
            .filter(|ext| !ext.is_empty() && digest.len() + 1 + ext.len() <= 255)
            .map(|ext| format!(".{ext}"))
            .unwrap_or_default();
        let tail = format!(" - {digest}{suffix}");
        if tail.len() >= 255 {
            return format!("{digest}{suffix}");
        }
        let mut end = 255 - tail.len();
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
        name.push_str(&tail);
    }
    name
}

pub(crate) fn artifact_name(identity: &Identity) -> String {
    let values = identity
        .keys
        .iter()
        .filter(|(key, value)| !value.is_empty() && !key.eq_ignore_ascii_case("company"))
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    component(&if values.is_empty() {
        identity.kind.clone()
    } else {
        values.join(" - ")
    })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {
    identity: Identity,
    path: String,
}

/// Cache verified names while the destination lock is held. Page collectors
/// commit one registry instead of two synchronized files for every artifact.
#[derive(Default)]
pub(crate) struct NameRegistry {
    loaded: bool,
    dirty: bool,
    identities: std::collections::BTreeMap<String, Name>,
    paths: std::collections::BTreeMap<String, String>,
}
impl NameRegistry {
    fn insert(&mut self, record: Name) -> Result<(), Diagnostic> {
        crate::storage::safe_relative(&record.path)?;
        let key = identity_key(&record.identity);
        let path_key = collision_key(&record.path);
        if self.paths.get(&path_key).is_some_and(|owner| owner != &key)
            || self.identities.get(&key).is_some_and(|old| old.path != record.path || old.identity != record.identity) {
            return Err(name_error());
        }
        self.paths.insert(path_key, key.clone());
        self.identities.insert(key, record);
        Ok(())
    }
    fn load(&mut self, storage: &Storage) -> Result<(), Diagnostic> {
        if self.loaded { return Ok(()); }
        let path = storage.confined("Herbert Data/names/pages.json")?;
        match fs::read(path) {
            Ok(bytes) => {
                let records: Vec<Name> = serde_json::from_slice(&bytes).map_err(|_| name_error())?;
                for record in records { self.insert(record)?; }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(name_error()),
        }
        self.loaded = true;
        Ok(())
    }
}

impl Storage {
    pub(crate) fn checkpoint_names(&self) -> Result<(), Diagnostic> {
        let mut names = self.output_names.lock().map_err(|_| name_error())?;
        if names.dirty {
            let bytes = serde_json::to_vec(&names.identities.values().collect::<Vec<_>>()).map_err(|_| name_error())?;
            atomic_write(&self.confined("Herbert Data/names/pages.json")?, &bytes)?;
            names.dirty = false;
        }
        Ok(())
    }
    pub(crate) fn artifact_folder(&self, identity: &Identity) -> Result<String, Diagnostic> {
        self.artifact_folder_in(identity, None, None)
    }

    /// Plugins can group related artifact families without changing identity.
    /// The saved naming registry remains authoritative for an existing output.
    pub(crate) fn group_artifact(
        &self,
        identity: &Identity,
        group: &str,
    ) -> Result<(), Diagnostic> {
        if group.is_empty() {
            return Err(name_error());
        }
        self.artifact_folder_in(identity, Some(group), None).map(|_| ())
    }

    /// Bind a caller-selected relative folder to a stable internal identity.
    pub(crate) fn artifact_folder_at(&self, identity:&Identity, path:&str)->Result<String,Diagnostic>{
        crate::storage::safe_relative(path)?;
        self.artifact_folder_in(identity,None,Some(path))
    }

    fn artifact_folder_in(
        &self,
        identity: &Identity,
        group: Option<&str>,
        requested: Option<&str>,
    ) -> Result<String, Diagnostic> {
        let mut names = self.output_names.lock().map_err(|_| {
            Diagnostic::new("OUTPUT_NAME_UNAVAILABLE", "Output naming is unavailable.")
        })?;
        names.load(self)?;
        let key = identity_key(identity);
        if let Some(record) = names.identities.get(&key) {
            if record.identity != *identity { return Err(name_error()); }
            return Ok(record.path.clone());
        }
        let pointer = self.confined(&format!(
            "Herbert Data/names/identities/{}.json",
            identity_key(identity)
        ))?;
        if pointer.exists() {
            let record: Name =
                serde_json::from_slice(&fs::read(&pointer).map_err(|_| name_error())?)
                    .map_err(|_| name_error())?;
            if record.identity != *identity {
                return Err(name_error());
            }
            crate::storage::safe_relative(&record.path)?;
            names.insert(record.clone())?;
            return Ok(record.path);
        }
        let mut category = component(
            self.journal
                .category_names
                .get(&identity.category)
                .unwrap_or(&identity.category),
        );
        if let Some(group) = group {
            category.push('/');
            category.push_str(&component(group));
        }
        let mut label = artifact_name(identity);
        match &identity.scope {
            Scope::Personal { company, owner } => {
                label.push_str(&format!(" - {} - {}", component(company), component(owner)))
            }
            Scope::Company { company } if company != &self.journal.company => {
                label.push_str(&format!(" - {}", component(company)))
            }
            _ => (),
        }
        label = bounded(label);
        let mut path = requested.map(str::to_owned).unwrap_or_else(||format!("{category}/{label}"));
        if let Some(requested)=requested {
            let (parent,name)=requested.rsplit_once('/').ok_or_else(name_error)?;
            category=parent.into();label=name.into();
        }
        let mut claim = self.confined(&format!(
            "Herbert Data/names/paths/{}.json",
            sha256(collision_key(&path).as_bytes())
        ))?;
        let existing = names.paths.get(&collision_key(&path))
            .and_then(|key| names.identities.get(key)).cloned();
        let existing = match existing {
            Some(record) => Some(record),
            None if claim.exists() => Some(serde_json::from_slice::<Name>(&fs::read(&claim).map_err(|_| name_error())?).map_err(|_| name_error())?),
            None => None,
        };
        if let Some(existing) = existing {
            if existing.identity != *identity {
                // Only an actual portable-name collision needs an identity suffix.
                path = format!(
                    "{category}/{}",
                    bounded(format!("{label} - {}", identity_key(identity)))
                );
                claim = self.confined(&format!(
                    "Herbert Data/names/paths/{}.json",
                    sha256(collision_key(&path).as_bytes())
                ))?;
            }
        }
        if claim.exists() {
            let existing: Name =
                serde_json::from_slice(&fs::read(&claim).map_err(|_| name_error())?)
                    .map_err(|_| name_error())?;
            if existing.identity != *identity
                || collision_key(&existing.path) != collision_key(&path)
            {
                return Err(name_error());
            }
        }
        let name = Name {
            identity: identity.clone(),
            path: path.clone(),
        };
        if self.page_owns_derived_files {
            names.insert(name.clone())?;
            names.dirty = true;
        } else {
            let record = serde_json::to_vec(&name).map_err(|_| name_error())?;
            atomic_write(&claim, &record)?;
            atomic_write(&pointer, &record)?;
            names.insert(name)?;
        }
        Ok(path)
    }
}
fn name_error() -> Diagnostic {
    Diagnostic::new(
        "OUTPUT_NAME_INVALID",
        "Saved output naming could not be verified. Existing files are preserved.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::new_journal;
    use std::collections::BTreeMap;
    fn id(name: &str) -> Identity {
        Identity {
            category: "C04".into(),
            kind: "BAQ".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([("QueryID".into(), name.into())]),
        }
    }
    #[test]
    fn readable_names_collisions_refresh_and_user_edits_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = new_journal("names".into());
        journal.category_names.insert("C04".into(), "BAQs".into());
        let store = Storage::open(temp.path(), false, journal.clone()).unwrap();
        let first = id("MyQuery");
        assert_eq!(store.artifact_folder(&first).unwrap(), "BAQs/MyQuery");
        assert_ne!(
            store.artifact_folder(&id("myquery")).unwrap(),
            "BAQs/myquery"
        );
        let (file, conflict) = store
            .write_derived(&first, "MyQuery.baq", b"native")
            .unwrap();
        assert!(!conflict);
        assert_eq!(file.path, "BAQs/MyQuery/MyQuery.baq");
        fs::write(store.confined(&file.path).unwrap(), b"user edit").unwrap();
        drop(store);
        let store = Storage::open(temp.path(), false, journal).unwrap();
        assert_eq!(store.artifact_folder(&first).unwrap(), "BAQs/MyQuery");
        let (server, conflict) = store
            .write_derived(&first, "MyQuery.baq", b"new native")
            .unwrap();
        assert!(conflict);
        assert!(server.path.starts_with("Conflicts/BAQs/MyQuery/"));
        assert_eq!(
            fs::read(store.confined(&file.path).unwrap()).unwrap(),
            b"user edit"
        );
        assert!(temp.path().join("Herbert Data").is_dir());
        assert!(!temp.path().join(".hoover").exists());
        assert!(!temp.path().join("raw").exists());
    }
    #[test]
    fn names_escape_paths_and_accommodate_long_identifiers() {
        assert_eq!(component("A/B"), "A%2FB");
        assert_eq!(component("CON"), "%5FCON");
        assert_ne!(component("A/B"), component("A%2FB"));
        let name = component(&format!("{}.baq", "漢".repeat(300)));
        assert!(name.len() <= 255 && name.ends_with(".baq"));
        assert_ne!(name, component(&format!("{}.baq", "字".repeat(300))));
    }

    #[test]
    fn plugin_groups_are_confined_and_keep_existing_names() {
        let temp = tempfile::tempdir().unwrap();
        let mut journal = new_journal("grouped".into());
        journal
            .category_names
            .insert("C04".into(), "Combined".into());
        let store = Storage::open(temp.path(), false, journal).unwrap();
        let first = id("First");
        store.group_artifact(&first, "Methods").unwrap();
        assert_eq!(
            store.artifact_folder(&first).unwrap(),
            "Combined/Methods/First"
        );
        store.group_artifact(&first, "Changed").unwrap();
        assert_eq!(
            store.artifact_folder(&first).unwrap(),
            "Combined/Methods/First"
        );
        let second = id("Second");
        store.group_artifact(&second, "../outside").unwrap();
        assert_eq!(
            store.artifact_folder(&second).unwrap(),
            "Combined/%2E.%2Foutside/Second"
        );
    }
    #[test]
    fn long_extensions_are_preserved_until_the_filesystem_boundary() {
        for extension in [
            "custom-definition-format".to_owned(),
            "定義形式".repeat(6),
            "x".repeat(190),
        ] {
            let original = format!("{}.{}", "漢".repeat(300), extension);
            let name = component(&original);
            assert!(name.len() <= 255 && name.encode_utf16().count() <= 255);
            assert!(name.ends_with(&format!(".{extension}")));
            assert!(name.contains(&sha256(original.as_bytes())));
        }
        let oversized = format!("{}.{}", "a".repeat(300), "x".repeat(191));
        let name = component(&oversized);
        assert!(name.len() <= 255);
        assert!(name.contains(&sha256(oversized.as_bytes())));
    }
    #[test]
    fn legacy_metadata_remains_readable_after_visible_folder_migration() {
        let temp = tempfile::tempdir().unwrap();
        let journal = new_journal("legacy".into());
        let store = Storage::open(temp.path(), false, journal.clone()).unwrap();
        drop(store);
        fs::rename(
            temp.path().join("Herbert Data"),
            temp.path().join(".hoover"),
        )
        .unwrap();
        let store = Storage::open(temp.path(), false, journal).unwrap();
        assert!(store.root().join("Herbert Data").exists());
        assert!(!store.root().join(".hoover").exists());
    }
}
