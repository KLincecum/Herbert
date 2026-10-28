//! Versioned input and credential-source selection.
//! Bind credentials to a validated endpoint. User-authored files remain read-only.
use herbert_contract::{Diagnostic, OperatingSettings};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum AuthMode {
    #[default]
    Session,
    BasicApiKey,
}

#[derive(Clone, Deserialize, Zeroize, ZeroizeOnDrop, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub api_key: Option<String>,
}
impl Credentials {
    pub fn validate(&self, mode: AuthMode) -> Result<(), Diagnostic> {
        if self.username.is_empty() {
            return Err(Diagnostic::new(
                "CREDENTIALS_REQUIRED",
                "Supply a username through the selected credential source. Native authentication determines whether the supplied password is valid.",
            ));
        }
        if mode == AuthMode::BasicApiKey && self.api_key.as_ref().is_none_or(|k| k.is_empty()) {
            return Err(Diagnostic::new(
                "API_KEY_REQUIRED",
                "Basic plus API key mode requires an API key.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Defaults {
    pub company: Option<String>,
    pub concurrency: Option<u8>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: Option<String>,
    pub url: String,
    #[serde(default)]
    pub auth_mode: AuthMode,
    #[serde(default)]
    pub defaults: Defaults,
    pub user_file: Option<UserFileRef>,
}
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserFileRef {
    pub path: String,
    pub environment_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileEnvironment {
    pub id: String,
    pub name: Option<String>,
    pub url: String,
    pub auth_mode: AuthMode,
    pub credentials: Credentials,
    #[serde(default)]
    pub defaults: Defaults,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentFile {
    pub schema_version: u32,
    pub environments: Vec<FileEnvironment>,
}

pub fn endpoint(value: &str) -> Result<Url, Diagnostic> {
    let mut url = Url::parse(value.trim()).map_err(|_| {
        Diagnostic::new(
            "INVALID_ENDPOINT",
            "Supply an absolute HTTP or HTTPS Epicor instance URL.",
        )
    })?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Diagnostic::new(
            "INVALID_ENDPOINT",
            "The instance URL must have a host and must not contain embedded credentials.",
        ));
    }
    // Browser and API URLs share the instance prefix. Match complete route
    // segments case-insensitively, without lowercasing the instance path.
    let path = url.path().to_owned();
    let segments: Vec<_> = path.split('/').collect();
    let mut offset = 0;
    for pair in segments.windows(2) {
        if (pair[0].eq_ignore_ascii_case("apps") && pair[1].eq_ignore_ascii_case("ERP"))
            || (pair[0].eq_ignore_ascii_case("api") && pair[1].eq_ignore_ascii_case("v2"))
        {
            url.set_path(&path[..offset]);
            break;
        }
        offset += pair[0].len() + 1;
    }
    // Browser navigation state is not part of the server/instance identity.
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

pub fn decode<T: for<'de> Deserialize<'de>>(value: serde_json::Value) -> Result<T, Diagnostic> {
    // Deliberately discard serde's input excerpts and arbitrary unknown field names.
    serde_json::from_value(value).map_err(|_| Diagnostic::new("INVALID_INPUT", "Input does not match the operation schema. Inspect schema for required fields and types."))
}

pub fn validate_profile(profile: &Profile) -> Result<(), Diagnostic> {
    if profile.id.is_empty() {
        return Err(Diagnostic::new(
            "INVALID_PROFILE",
            "The profile id must be nonempty.",
        ));
    }
    endpoint(&profile.url)?;
    if let Some(c) = profile.defaults.concurrency {
        OperatingSettings {
            concurrency: c,
            ..Default::default()
        }
        .validate()?;
    }
    Ok(())
}

async fn read_environment_document(path: &Path) -> Result<EnvironmentFile, Diagnostic> {
    let bytes = tokio::fs::read(path).await.map_err(|_| {
        Diagnostic::new(
            "ENVIRONMENT_FILE_UNAVAILABLE",
            "The selected environment input file cannot be read.",
        )
    })?;
    let raw = zeroize::Zeroizing::new(bytes);
    let file: EnvironmentFile = serde_json::from_slice(&raw).map_err(|_| {
        Diagnostic::new(
            "INVALID_ENVIRONMENT_FILE",
            "The environment input file has invalid JSON, fields or types.",
        )
    })?;
    if file.schema_version != 1 {
        return Err(Diagnostic::new(
            "UNKNOWN_SCHEMA",
            "The environment file schemaVersion must be 1.",
        ));
    }
    let mut ids = BTreeSet::new();
    for env in &file.environments {
        if env.id.is_empty() || !ids.insert(&env.id) {
            return Err(Diagnostic::new(
                "DUPLICATE_ENVIRONMENT_ID",
                "Environment IDs must be nonempty and unique with exact case.",
            ));
        }
        endpoint(&env.url)?;
        env.credentials.validate(env.auth_mode)?;
        if let Some(c) = env.defaults.concurrency {
            OperatingSettings {
                concurrency: c,
                ..Default::default()
            }
            .validate()?;
        }
    }
    Ok(file)
}

/// Return only connection labels for the file chooser. Discard credential values.
pub async fn environment_choices(path: &Path) -> Result<Vec<Profile>, Diagnostic> {
    Ok(read_environment_document(path)
        .await?
        .environments
        .into_iter()
        .map(|environment| Profile {
            id: environment.id,
            name: environment.name,
            url: environment.url,
            auth_mode: environment.auth_mode,
            defaults: environment.defaults,
            user_file: None,
        })
        .collect())
}

pub async fn read_environment_file(
    path: &Path,
    selected: &str,
) -> Result<FileEnvironment, Diagnostic> {
    read_environment_document(path)
        .await?
        .environments
        .into_iter()
        .find(|e| e.id == selected)
        .ok_or_else(|| {
            Diagnostic::new(
                "UNKNOWN_ENVIRONMENT_ID",
                "The exact selected environment ID does not exist in the input file.",
            )
        })
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum CredentialSource {
    Inline {
        credentials: Credentials,
    },
    UserFile {
        path: String,
        environment_id: String,
    },
    SecureStore,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInput {
    pub profile_id: Option<String>,
    pub url: Option<String>,
    pub auth_mode: Option<AuthMode>,
    pub company: Option<String>,
    pub credential_source: CredentialSource,
    pub settings: Option<OperatingSettings>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_credential_urls_and_retains_instance_binding() {
        assert!(endpoint("https://user:secret@example.test/instance").is_err());
        assert_eq!(
            endpoint("https://EXAMPLE.test/instance").unwrap().as_str(),
            "https://example.test/instance/"
        );
        assert_ne!(
            endpoint("https://example.test/A").unwrap(),
            endpoint("https://example.test/B").unwrap()
        );
    }
    #[test]
    fn pasted_browser_and_api_urls_preserve_instance_case() {
        for suffix in ["apps/ERP", "APPS/erp/SomeScreen", "aPpS/eRp/", "api/v2", "API/V2/odata/C/Ice.BO.TestSvc/GetList"] {
            let pasted = format!("  HTTPS://EXAMPLE.test:9448/MyInstance/{suffix}?company=C#screen  ");
            let normalized = endpoint(&pasted).unwrap();
            assert_eq!(normalized.as_str(), "https://example.test:9448/MyInstance/");
            assert_eq!(endpoint(normalized.as_str()).unwrap(), normalized);
            assert_eq!(normalized.join("api/TokenResource").unwrap().path(), "/MyInstance/api/TokenResource");
        }
    }
    #[test]
    fn url_suffix_matching_requires_whole_segments_and_preserves_encoded_prefix() {
        for path in ["/MyApps/ERP", "/Instance/apps/ERPExtra", "/Instance/api/v20", "/MyApi/v2Test"] {
            assert_eq!(endpoint(&format!("https://example.test{path}")).unwrap().path(), format!("{path}/"));
        }
        assert_eq!(endpoint("https://example.test/Team%20One/MyInstance/ApPs/ErP").unwrap().path(), "/Team%20One/MyInstance/");
        assert_eq!(endpoint("https://example.test/API/v2/odata").unwrap().path(), "/");
        assert_eq!(endpoint("https://example.test/Instance?company=C#screen").unwrap().path(), "/Instance/");
        assert!(endpoint("https://user:secret@example.test/Instance/apps/ERP").is_err());
    }
    #[tokio::test]
    async fn file_is_read_only_exact_selection_and_no_secret_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("input.json");
        let text = r#"{"schemaVersion":1,"environments":[{"id":"A","url":"https://example.test/x","authMode":"session","credentials":{"username":"u","password":"  secret  "}},{"id":"a","url":"https://example.test/y","authMode":"session","credentials":{"username":"u","password":"p"}}]}"#;
        std::fs::write(&p, text).unwrap();
        let choices = environment_choices(&p).await.unwrap();
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            ["A", "a"]
        );
        let projection = serde_json::to_string(&choices).unwrap();
        assert!(!projection.contains("secret"));
        assert!(!projection.contains("credentials"));
        assert!(!projection.contains("password"));
        assert_eq!(
            read_environment_file(&p, "A")
                .await
                .unwrap()
                .credentials
                .password,
            "  secret  "
        );
        assert_eq!(
            read_environment_file(&p, "a").await.unwrap().url,
            "https://example.test/y"
        );
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        assert_eq!(
            read_environment_file(&p, "missing")
                .await
                .err()
                .unwrap()
                .code,
            "UNKNOWN_ENVIRONMENT_ID"
        );
        let duplicate = text.replace("\"id\":\"a\"", "\"id\":\"A\"");
        std::fs::write(&p, duplicate).unwrap();
        assert_eq!(
            environment_choices(&p).await.unwrap_err().code,
            "DUPLICATE_ENVIRONMENT_ID"
        );
        assert_eq!(
            read_environment_file(&p, "A").await.err().unwrap().code,
            "DUPLICATE_ENVIRONMENT_ID"
        );
        std::fs::write(
            &p,
            text.replace(
                "\"password\":\"  secret  \"",
                "\"bad-secret-field\":\"  secret  \"",
            ),
        )
        .unwrap();
        let e = read_environment_file(&p, "A").await.err().unwrap();
        assert!(!serde_json::to_string(&e).unwrap().contains("secret"));
    }
}
