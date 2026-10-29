//! Ordinary plugin.json manifests and operation-local native export snapshots.
use herbert_contract::{Diagnostic, NativeArtifactDeclaration, PluginManifest};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub(crate) struct NativeArtifactSnapshot {
    pub declaration: NativeArtifactDeclaration,
    /// Every declared candidate is read once at operation start. Selection after
    /// native-version discovery never reopens a mutable author file.
    pub files: BTreeMap<String, Arc<Vec<u8>>>,
}
#[cfg(test)]
impl NativeArtifactSnapshot {
    pub fn bytes_for_version(&self, version: &str) -> Result<Arc<Vec<u8>>, Diagnostic> {
        let file = self.declaration.file_for_version(version);
        self.files.get(file).cloned().ok_or_else(|| {
            problem(
                "PLUGIN_ARTIFACT_SNAPSHOT_INVALID",
                format!(
                    "Artifact {} has no snapshotted file for the selected Epicor version.",
                    self.declaration.alias
                ),
            )
        })
    }
}
fn problem(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn directory(module: &Path) -> Result<&Path, Diagnostic> {
    module
        .parent()
        .map(|folder| {
            if folder.as_os_str().is_empty() {
                Path::new(".")
            } else {
                folder
            }
        })
        .ok_or_else(|| {
            problem(
                "PLUGIN_ARTIFACT_PATH_INVALID",
                "The module has no package directory.",
            )
        })
}
async fn contained_file(root: &Path, name: &str) -> Result<PathBuf, Diagnostic> {
    let relative = crate::storage::safe_relative(name).map_err(|_| {
        problem(
            "PLUGIN_ARTIFACT_PATH_INVALID",
            format!("Artifact file {name} must be a relative path inside the plugin package."),
        )
    })?;
    let resolved = tokio::fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| {
            problem(
                "PLUGIN_ARTIFACT_FILE_UNAVAILABLE",
                format!("Cannot read declared artifact file {name}."),
            )
        })?;
    if !resolved.starts_with(root)
        || !tokio::fs::metadata(&resolved)
            .await
            .map_err(|_| {
                problem(
                    "PLUGIN_ARTIFACT_FILE_UNAVAILABLE",
                    format!("Cannot inspect declared artifact file {name}."),
                )
            })?
            .is_file()
    {
        return Err(problem(
            "PLUGIN_ARTIFACT_PATH_INVALID",
            format!("Artifact file {name} must resolve to a file inside the plugin package."),
        ));
    }
    Ok(resolved)
}
pub(crate) async fn manifest(module: &Path) -> Result<PluginManifest, Diagnostic> {
    let folder = directory(module)?;
    let file = folder.join("plugin.json");
    let bytes = match tokio::fs::read(&file).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PluginManifest::default());
        }
        Err(_) => {
            return Err(problem(
                "PLUGIN_ARTIFACT_MANIFEST_UNAVAILABLE",
                "Cannot read the plugin's sibling plugin.json manifest.",
            ));
        }
    };
    let manifest: PluginManifest = serde_json::from_slice(&bytes).map_err(|_| {
        problem(
            "PLUGIN_ARTIFACT_MANIFEST_INVALID",
            "The plugin.json manifest does not match the Epicor artifact declaration format.",
        )
    })?;
    manifest.ordered_artifacts()?;
    for artifact in &manifest.artifacts {
        for file in std::iter::once(&artifact.file)
            .chain(artifact.version_files.iter().map(|file| &file.file))
        {
            crate::storage::safe_relative(file).map_err(|_| {
                problem(
                    "PLUGIN_ARTIFACT_PATH_INVALID",
                    format!(
                        "Artifact {} declares a path outside the plugin package.",
                        artifact.alias
                    ),
                )
            })?;
        }
    }
    Ok(manifest)
}
pub(crate) async fn snapshot(module: &Path) -> Result<Vec<NativeArtifactSnapshot>, Diagnostic> {
    let manifest = manifest(module).await?;
    let declarations = manifest.ordered_artifacts()?;
    if declarations.is_empty() {
        return Ok(Vec::new());
    }
    let folder = tokio::fs::canonicalize(directory(module)?)
        .await
        .map_err(|_| {
            problem(
                "PLUGIN_ARTIFACT_FOLDER_UNAVAILABLE",
                "The plugin package directory is unavailable.",
            )
        })?;
    let mut shared = BTreeMap::<String, Arc<Vec<u8>>>::new();
    let mut snapshots = Vec::new();
    for declaration in declarations {
        let mut files = BTreeMap::new();
        for file in std::iter::once(&declaration.file)
            .chain(declaration.version_files.iter().map(|file| &file.file))
        {
            let bytes = if let Some(bytes) = shared.get(file) {
                bytes.clone()
            } else {
                let resolved = contained_file(&folder, file).await?;
                let bytes = Arc::new(tokio::fs::read(resolved).await.map_err(|_| {
                    problem(
                        "PLUGIN_ARTIFACT_FILE_UNAVAILABLE",
                        format!(
                            "Cannot snapshot artifact {} file {file}.",
                            declaration.alias
                        ),
                    )
                })?);
                shared.insert(file.clone(), bytes.clone());
                bytes
            };
            files.insert(file.clone(), bytes);
        }
        snapshots.push(NativeArtifactSnapshot { declaration, files });
    }
    Ok(snapshots)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    async fn write_manifest(folder: &Path, value: serde_json::Value) {
        tokio::fs::write(
            folder.join("plugin.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .await
        .unwrap();
    }
    fn declaration(alias: &str, file: &str) -> serde_json::Value {
        json!({"alias":alias,"type":"functionLibrary","file":file,"lifetime":"temporary","policy":"createInstance","effects":{"description":"Installs a temporary Function library, downloads records, then removes the library."}})
    }
    #[tokio::test]
    async fn file_and_manifest_edits_apply_to_the_next_snapshot_without_module_rebuild() {
        let folder = tempfile::tempdir().unwrap();
        let module = folder.path().join("plugin.wasm");
        tokio::fs::write(&module, b"module unchanged")
            .await
            .unwrap();
        tokio::fs::write(folder.path().join("default.efxj"), b"first")
            .await
            .unwrap();
        tokio::fs::write(folder.path().join("specific.efxj"), b"specific first")
            .await
            .unwrap();
        let mut artifact = declaration("helper", "default.efxj");
        artifact["versionFiles"] = json!([{"epicorVersion":"12.2.100.9", "file":"specific.efxj"}]);
        write_manifest(folder.path(), json!({"artifacts":[artifact.clone()]})).await;
        let first = snapshot(&module).await.unwrap();
        tokio::fs::write(folder.path().join("default.efxj"), b"second")
            .await
            .unwrap();
        tokio::fs::write(folder.path().join("specific.efxj"), b"specific second")
            .await
            .unwrap();
        artifact["effects"]["description"] = json!("Changed operation description");
        write_manifest(folder.path(), json!({"artifacts":[artifact]})).await;
        let second = snapshot(&module).await.unwrap();
        assert_eq!(
            first[0].bytes_for_version("other").unwrap().as_slice(),
            b"first"
        );
        assert_eq!(
            first[0].bytes_for_version("12.2.100.9").unwrap().as_slice(),
            b"specific first"
        );
        assert_eq!(
            second[0].bytes_for_version("other").unwrap().as_slice(),
            b"second"
        );
        assert_eq!(
            second[0]
                .bytes_for_version("12.2.100.9")
                .unwrap()
                .as_slice(),
            b"specific second"
        );
        assert_ne!(
            first[0].declaration.effects.description,
            second[0].declaration.effects.description
        );
        assert_eq!(tokio::fs::read(&module).await.unwrap(), b"module unchanged");
    }
    #[tokio::test]
    async fn absent_manifest_is_compatible_and_paths_cannot_escape_package() {
        let folder = tempfile::tempdir().unwrap();
        let module = folder.path().join("plugin.wasm");
        assert!(snapshot(&module).await.unwrap().is_empty());
        for file in ["../outside.efxj", "C:/outside.efxj", "/outside.efxj"] {
            write_manifest(
                folder.path(),
                json!({"artifacts":[declaration("helper",file)]}),
            )
            .await;
            assert_eq!(
                snapshot(&module).await.unwrap_err().code,
                "PLUGIN_ARTIFACT_PATH_INVALID"
            );
        }
        write_manifest(
            folder.path(),
            json!({"artifacts":[declaration("helper","missing.efxj")]}),
        )
        .await;
        assert_eq!(
            snapshot(&module).await.unwrap_err().code,
            "PLUGIN_ARTIFACT_FILE_UNAVAILABLE"
        );
    }
    #[tokio::test]
    async fn all_version_candidates_are_required_before_an_operation_can_start() {
        let folder = tempfile::tempdir().unwrap();
        let module = folder.path().join("plugin.wasm");
        tokio::fs::write(folder.path().join("default.efxj"), b"native package")
            .await
            .unwrap();
        let mut artifact = declaration("helper", "default.efxj");
        artifact["versionFiles"] = json!([{"epicorVersion":"12.2.100.9","file":"missing.efxj"}]);
        write_manifest(folder.path(), json!({"artifacts":[artifact]})).await;
        assert_eq!(
            snapshot(&module).await.unwrap_err().code,
            "PLUGIN_ARTIFACT_FILE_UNAVAILABLE"
        );
    }
}
