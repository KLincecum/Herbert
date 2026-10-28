//! Narrow native adapters for declared Function libraries, BAQs and UBAQs.
//! Lifecycle intent, backups and retry decisions belong to the coordinator.
use crate::{
    connection::{Connection, NativeResponse},
    native_artifact_store::NativeIdentity,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use herbert_contract::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
pub(crate) const EFX: &str = "Ice.Lib.EfxLibraryDesignerSvc";
pub(crate) const DESIGNER: &str = "Ice.BO.BAQDesignerSvc";
pub(crate) const QUERY: &str = "Ice.BO.DynamicQuerySvc";
fn problem(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn rows<'a>(value: &'a Value, table: &str) -> Result<&'a Vec<Value>, Diagnostic> {
    value
        .get("returnObj")
        .unwrap_or(value)
        .get(table)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            problem(
                "ARTIFACT_READBACK_INVALID",
                format!("Native response omitted {table}."),
            )
        })
}
fn string(row: &Value, field: &str) -> Result<String, Diagnostic> {
    row[field].as_str().map(String::from).ok_or_else(|| {
        problem(
            "ARTIFACT_READBACK_INVALID",
            format!("Native response omitted {field}."),
        )
    })
}
pub(crate) async fn probe(
    connection: &Connection,
    kind: NativeArtifactType,
    id: &str,
    cancel: &CancellationToken,
) -> Result<Option<(NativeIdentity, Value)>, Diagnostic> {
    let (native, table, id_field, marker_field) = if kind == NativeArtifactType::FunctionLibrary {
        (
            connection
                .artifact_call(EFX, "GetLibrary", json!({"libraryId":id}), cancel)
                .await?,
            "EfxLibrary",
            "LibraryID",
            "Notes",
        )
    } else {
        let list=connection.artifact_call(DESIGNER,"GetList",json!({"whereClause":format!("QueryID = '{}'",id.replace('\'',"''")),"pageSize":0,"absolutePage":1}),cancel).await?;
        let found = rows(&list.value, "DynamicQueryDesignerList")?;
        if found.is_empty() {
            if list
                .value
                .get("parameters")
                .and_then(|p| p.get("morePages"))
                .or_else(|| list.value.get("morePages"))
                .and_then(Value::as_bool)
                != Some(false)
            {
                return Err(problem(
                    "ARTIFACT_ABSENCE_UNVERIFIED",
                    "The native query list did not prove complete absence.",
                ));
            }
            return Ok(None);
        }
        if found.iter().any(|r| r["QueryID"].as_str() != Some(id)) {
            return Err(problem(
                "ARTIFACT_IDENTITY_MISMATCH",
                "The native exact-name list returned another query.",
            ));
        }
        (
            connection
                .artifact_call(DESIGNER, "GetByID", json!({"queryID":id}), cancel)
                .await?,
            "DynamicQueryDesigner",
            "QueryID",
            "Comment",
        )
    };
    let found = rows(&native.value, table)?;
    if found.is_empty() {
        return Ok(None);
    }
    if found.len() != 1 || found[0][id_field].as_str() != Some(id) {
        return Err(problem(
            "ARTIFACT_IDENTITY_MISMATCH",
            "Native readback did not identify one exact installed artifact.",
        ));
    }
    let row = &found[0];
    let company = if kind == NativeArtifactType::FunctionLibrary {
        string(row, "OwnedByCompany")?
    } else {
        string(row, "Company")?
    };
    let row_id = string(row, "SysRowID")?;
    if uuid::Uuid::parse_str(&row_id).is_err() {
        return Err(problem(
            "ARTIFACT_IDENTITY_MISMATCH",
            "The native artifact row identity is invalid.",
        ));
    }
    Ok(Some((
        NativeIdentity {
            id: id.into(),
            company,
            row_id,
            global_id: if kind == NativeArtifactType::FunctionLibrary {
                Some(string(row, "GlobalID")?)
            } else {
                None
            },
            owner: Some(string(
                row,
                if kind == NativeArtifactType::FunctionLibrary {
                    "Owner"
                } else {
                    "AuthorID"
                },
            )?),
            updatable: if kind == NativeArtifactType::FunctionLibrary {
                None
            } else {
                Some(row["Updatable"].as_bool().ok_or_else(|| {
                    problem(
                        "ARTIFACT_READBACK_INVALID",
                        "The native query omits its update capability.",
                    )
                })?)
            },
            marker: string(row, marker_field)?,
        },
        native.value,
    )))
}
pub(crate) fn owns(identity: &NativeIdentity, expected: &NativeIdentity) -> bool {
    identity == expected
}
pub(crate) async fn export(
    connection: &Connection,
    kind: NativeArtifactType,
    id: &str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, Diagnostic> {
    if kind == NativeArtifactType::FunctionLibrary {
        let response=connection.artifact_call(EFX,"ExportLibrary",json!({"libraryID":id,"options":{"Mode":0,"Format":1,"Package":"","PackageVersion":"","Publisher":"","InstallAsHidden":false}}),cancel).await?;
        crate::native_artifact_backend::library_backup(&response.value, id).map(|(bytes, _)| bytes)
    } else {
        let response = connection
            .artifact_call(
                DESIGNER,
                "ExportBaq",
                json!({"queryId":id,"options":{}}),
                cancel,
            )
            .await?;
        STANDARD
            .decode(response.value["returnObj"].as_str().ok_or_else(|| {
                problem(
                    "ARTIFACT_EXPORT_INVALID",
                    "The native query export omitted its package bytes.",
                )
            })?)
            .map_err(|_| {
                problem(
                    "ARTIFACT_EXPORT_INVALID",
                    "The native query export contained invalid base64.",
                )
            })
    }
}
pub(crate) async fn import(
    connection: &Connection,
    kind: NativeArtifactType,
    id: &str,
    bytes: &[u8],
    replace: bool,
    cancel: &CancellationToken,
) -> Result<NativeResponse, Diagnostic> {
    let response = if kind == NativeArtifactType::FunctionLibrary {
        connection.artifact_call(EFX,"ImportLibrary",json!({"libraryPackage":STANDARD.encode(bytes),"options":{"NewLibraryId":id,"OverwriteMode":if replace {2}else{0},"IsProtected":false}}),cancel).await?
    } else {
        // The native importer asks each concrete overwrite question separately.
        // Never approve masking or changing another company's global query.
        let decision = if replace { "true" } else { "false" };
        connection.artifact_call(DESIGNER,"ImportBaq",json!({"newQueryId":id,"importBytes":STANDARD.encode(bytes),"options":{
            "ImportQueryInCurrentCompany":"true","SameIDInCurrentCompany":decision,"NonGlobalInCurrentCompany":decision,"GlobalInCurrentCompany":decision,
            "GsmUserOverwritesAnothersQuery":decision,"MaskAnotherGlobalWithCurrentLocal":"false",
            "ConflictGlobalsStoreAsLocal":"false","MaskAnotherLocalWithCurrentGlobal":"false"
        }}),cancel).await?
    };
    if kind != NativeArtifactType::FunctionLibrary
        && response.value["returnObj"].as_bool() != Some(true)
    {
        return Err(problem(
            "ARTIFACT_IMPORT_REFUSED",
            format!(
                "The native BAQ importer refused the package. Native log: {}. Native decisions: {}. Target readback is required before another write.",
                response
                    .value
                    .pointer("/parameters/logResult")
                    .or_else(|| response.value.get("logResult"))
                    .unwrap_or(&Value::Null),
                response
                    .value
                    .pointer("/parameters/options")
                    .or_else(|| response.value.get("options"))
                    .unwrap_or(&Value::Null)
            ),
        ));
    }
    if kind == NativeArtifactType::FunctionLibrary {
        let errors = response
            .value
            .pointer("/returnObj/BOUpdError")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                problem(
                    "ARTIFACT_IMPORT_UNVERIFIED",
                    "Native Function import omitted its diagnostic table. Readback is required.",
                )
            })?;
        if !errors.is_empty() {
            return Err(problem(
                "ARTIFACT_IMPORT_DIAGNOSTICS",
                format!(
                    "Native Function import returned diagnostics: {}",
                    serde_json::to_string(errors).unwrap_or_default()
                ),
            ));
        }
    }
    Ok(response)
}
pub(crate) fn verify_query(
    declaration: &NativeArtifactDeclaration,
    readback: &Value,
) -> Result<(), Diagnostic> {
    let roots = rows(readback, "DynamicQueryDesigner")?;
    if roots.len() != 1
        || roots[0]["Updatable"].as_bool()
            != Some(declaration.artifact_type == NativeArtifactType::Ubaq)
    {
        return Err(problem(
            "ARTIFACT_UBAQ_DOWNGRADED",
            "The imported query does not retain its declared BAQ/UBAQ behavior. Epicor may have retried a failed UBAQ import as an ordinary query.",
        ));
    }
    for required in &declaration.required_query_parameters {
        let controls = rows(readback, "QueryCtrlDesigner")?;
        let matches = controls
            .iter()
            .filter(|p| p["DataSource"].as_str() == Some(&required.name))
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0]["DataType"].as_str() != Some(&required.data_type)
            || matches[0]["IsMandatory"].as_bool() != Some(required.required)
        {
            return Err(problem(
                "ARTIFACT_QUERY_SIGNATURE_MISMATCH",
                format!(
                    "Query parameter {} does not match its required native type and required flag.",
                    required.name
                ),
            ));
        }
    }
    Ok(())
}
pub(crate) fn verify_functions(
    declaration: &NativeArtifactDeclaration,
    package: &Value,
    active: bool,
) -> Result<(), Diagnostic> {
    let library = &package["Library"];
    if active
        && (library["Disabled"].as_bool() != Some(false)
            || library["Published"].as_bool() != Some(true))
    {
        return Err(problem(
            "ARTIFACT_FUNCTION_NOT_ACTIVE",
            "The native Function library is not enabled and published.",
        ));
    }
    let functions = library["Functions"].as_array().ok_or_else(|| {
        problem(
            "ARTIFACT_READBACK_INVALID",
            "The Function export omitted its functions.",
        )
    })?;
    for required in &declaration.required_functions {
        let found = functions
            .iter()
            .filter(|f| f["FunctionID"].as_str() == Some(&required.function))
            .collect::<Vec<_>>();
        if found.len() != 1 {
            return Err(problem(
                "ARTIFACT_FUNCTION_SIGNATURE_MISMATCH",
                format!(
                    "Required function {} is absent or ambiguous.",
                    required.function
                ),
            ));
        }
        let function = found[0];
        if active
            && (function["Disabled"].as_bool() != Some(false)
                || function["Invalid"].as_bool() != Some(false))
        {
            return Err(problem(
                "ARTIFACT_COMPILATION_FAILED",
                format!(
                    "Function {} is disabled or invalid after native compilation.",
                    required.function
                ),
            ));
        }
        let signature = function["FunctionSignature"].as_array().ok_or_else(|| {
            problem(
                "ARTIFACT_FUNCTION_SIGNATURE_MISMATCH",
                "Native function signature is missing.",
            )
        })?;
        let mut expected = Vec::new();
        for p in &required.parameters {
            if p.direction != NativeArtifactParameterDirection::Output {
                expected.push((&p.name, &p.data_type, false));
            }
            if p.direction != NativeArtifactParameterDirection::Input {
                expected.push((&p.name, &p.data_type, true));
            }
        }
        if signature.len() != expected.len()
            || expected.iter().any(|(name, ty, response)| {
                signature
                    .iter()
                    .filter(|p| {
                        p["ArgumentName"].as_str() == Some(name.as_str())
                            && p["DataType"].as_str() == Some(ty.as_str())
                            && p["Response"].as_bool() == Some(*response)
                    })
                    .count()
                    != 1
            })
        {
            return Err(problem(
                "ARTIFACT_FUNCTION_SIGNATURE_MISMATCH",
                format!(
                    "Function {} does not match its required native parameter signature.",
                    required.function
                ),
            ));
        }
    }
    Ok(())
}
pub(crate) async fn enable(
    connection: &Connection,
    id: &str,
    expected: &NativeIdentity,
    source: &Value,
    cancel: &CancellationToken,
) -> Result<(), Diagnostic> {
    let (current, native) = probe(connection, NativeArtifactType::FunctionLibrary, id, cancel)
        .await?
        .ok_or_else(|| {
            problem(
                "ARTIFACT_NOT_FOUND",
                "The imported Function library is absent.",
            )
        })?;
    if !owns(&current, expected) {
        return Err(problem(
            "ARTIFACT_OWNER_MISMATCH",
            "The Function library changed ownership before compilation.",
        ));
    }
    let mut tableset = native["returnObj"].clone();
    for table in ["EfxLibrary", "EfxFunction"] {
        for row in tableset[table].as_array_mut().ok_or_else(|| {
            problem(
                "ARTIFACT_READBACK_INVALID",
                "Native enable rows are absent.",
            )
        })? {
            if row["LibraryID"].as_str() != Some(id) {
                return Err(problem(
                    "ARTIFACT_OWNER_MISMATCH",
                    "A native compilation row belongs to another library.",
                ));
            }
            let disabled = if table == "EfxLibrary" {
                source["Library"]["Disabled"].as_bool()
            } else {
                source["Library"]["Functions"]
                    .as_array()
                    .and_then(|f| f.iter().find(|f| f["FunctionID"] == row["FunctionID"]))
                    .and_then(|f| f["Disabled"].as_bool())
            }
            .ok_or_else(|| {
                problem(
                    "ARTIFACT_PACKAGE_INVALID",
                    "The source omits its intended enabled/disabled state.",
                )
            })?;
            row["Disabled"] = json!(disabled);
            row["RowMod"] = json!("U");
        }
    }
    let response = connection
        .artifact_call(
            EFX,
            "ApplyChangesWithDiagnostics",
            json!({"libraryTableset":tableset}),
            cancel,
        )
        .await?;
    crate::native_access::verify_update_result(&response.value)
}
pub(crate) async fn publish(
    connection: &Connection,
    id: &str,
    cancel: &CancellationToken,
) -> Result<(), Diagnostic> {
    connection
        .artifact_call(EFX, "PromoteToProduction", json!({"libraryID":id}), cancel)
        .await
        .map(|_| ())
}
pub(crate) fn verify_compiled(package: &Value, source: &Value) -> Result<(), Diagnostic> {
    let functions = package["Library"]["Functions"].as_array().ok_or_else(|| {
        problem(
            "ARTIFACT_COMPILATION_UNVERIFIED",
            "Native compiled function rows are absent.",
        )
    })?;
    let originals = source["Library"]["Functions"].as_array().ok_or_else(|| {
        problem(
            "ARTIFACT_PACKAGE_INVALID",
            "Source function rows are absent.",
        )
    })?;
    for original in originals {
        let matches = functions
            .iter()
            .filter(|f| f["FunctionID"] == original["FunctionID"])
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0]["Disabled"] != original["Disabled"]
            || (original["Disabled"].as_bool() == Some(false)
                && matches[0]["Invalid"].as_bool() != Some(false))
        {
            return Err(problem(
                "ARTIFACT_COMPILATION_FAILED",
                format!(
                    "Function {} did not retain its declared enabled state or did not compile.",
                    original["FunctionID"].as_str().unwrap_or("unknown")
                ),
            ));
        }
    }
    Ok(())
}
pub(crate) async fn delete(
    connection: &Connection,
    kind: NativeArtifactType,
    expected: &NativeIdentity,
    cancel: &CancellationToken,
) -> Result<(), Diagnostic> {
    let Some((current, native)) = probe(connection, kind, &expected.id, cancel).await? else {
        return Ok(());
    };
    if !owns(&current, expected) {
        return Err(problem(
            "ARTIFACT_OWNER_MISMATCH",
            "The native identity or ownership marker changed. The artifact was preserved.",
        ));
    }
    let result = if kind == NativeArtifactType::FunctionLibrary {
        let mut row = rows(&native, "EfxLibrary")?[0].clone();
        row["RowMod"] = json!("D");
        connection
            .artifact_call(
                EFX,
                "ApplyChangesWithDiagnostics",
                json!({"libraryTableset":{"EfxLibrary":[row]}}),
                cancel,
            )
            .await
    } else {
        if current.company != connection.company {
            return Err(problem(
                "ARTIFACT_COMPANY_MISMATCH",
                "The query delete route cannot target a different company's artifact.",
            ));
        }
        connection
            .artifact_call(
                DESIGNER,
                "DeleteByID",
                json!({"queryID":expected.id}),
                cancel,
            )
            .await
    };
    if probe(connection, kind, &expected.id, &CancellationToken::new())
        .await?
        .is_some()
    {
        return Err(result.err().unwrap_or_else(|| {
            problem(
                "ARTIFACT_CLEANUP_PENDING",
                "The temporary artifact is still present after native cleanup.",
            )
        }));
    }
    Ok(())
}

/// Parse a native backup for verification of a plugin-declared installed helper.
/// Collection plugins parse their own exports through the generic JSON action.
pub(crate) fn library_backup(response: &Value, library: &str) -> Result<(Vec<u8>, Value), Diagnostic> {
    use base64::Engine;
    let invalid = || Diagnostic::new("NATIVE_EXPORT_INVALID", "The native helper backup has invalid encoding, structure, or identity.");
    let encoded = response.get("returnObj").and_then(Value::as_str).ok_or_else(invalid)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| invalid())?;
    let view = bytes.strip_prefix(&[0xEF,0xBB,0xBF]).unwrap_or(&bytes);
    let value: Value = serde_json::from_slice(view).map_err(|_| invalid())?;
    if value.get("LibraryId").and_then(Value::as_str) != Some(library) || !value.get("Library").is_some_and(Value::is_object) || value["Mode"] != "Backup" { return Err(invalid()); }
    Ok((bytes,value))
}
