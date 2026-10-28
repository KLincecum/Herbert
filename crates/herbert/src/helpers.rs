//! Generic exact-source contracts advertised by installed optional modules.
//! No helper library, function, filename or collection ID is registered here.
use herbert_contract::{Diagnostic, HelperContract, HelperEffect, Identity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) struct VerifiedHelper {
    pub library: String,
    pub function: String,
    pub company: String,
    pub company_argument: String,
    pub effect: HelperEffect,
}
/// Operational ownership evidence, retained independently of module discovery.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HelperTransfer {
    pub operation_id: String,
    pub identity: Identity,
    pub claim: HelperContract,
    pub function: String,
    pub close_arguments: Value,
    pub phase: String,
    pub plugin_hash: String,
    #[serde(default)]
    pub dispatch_count: u64,
}
fn refused() -> Diagnostic {
    Diagnostic::new(
        "HELPER_CONTRACT_MISMATCH",
        "The native helper identity, business-source governance, signature or exact source does not match the reviewed contract.",
    )
}
pub(crate) fn verify(
    claim: &HelperContract,
    function: &str,
    package: &Value,
    company: &str,
) -> Result<VerifiedHelper, Diagnostic> {
    verify_package(claim, function, package, company, true)
}

pub(crate) fn verify_package(
    claim: &HelperContract,
    function: &str,
    package: &Value,
    company: &str,
    require_published: bool,
) -> Result<VerifiedHelper, Diagnostic> {
    let library = package.get("Library").ok_or_else(refused)?;
    if package["LibraryId"].as_str() != Some(claim.library_id.as_str())
        || library["GlobalID"]
            .as_str()
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            != uuid::Uuid::parse_str(&claim.global_id).ok()
        || uuid::Uuid::parse_str(&claim.global_id).is_err()
        || !matches!(library["DirectDBAccess"].as_u64(), Some(0 | 1))
        || library["AllowCustomCodeWidgets"].as_bool() != Some(false)
        || library["Disabled"].as_bool().is_none()
        || (require_published && library["Disabled"].as_bool() != Some(false))
        || (require_published && library["Published"].as_bool() != Some(true))
        || library["Private"].as_bool() != Some(false)
    {
        return Err(refused());
    }
    if !library["LibraryMappings"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["Company"].as_str() == Some(company) && row["Allowed"].as_bool() == Some(true)
        })
    }) {
        return Err(refused());
    }
    if library["LibraryReferences"].as_array().is_none_or(|rows| {
        rows.iter()
            .any(|row| row["Updatable"].as_bool() == Some(true))
    }) {
        return Err(refused());
    }
    let functions = library["Functions"].as_array().ok_or_else(refused)?;
    if functions.len() != claim.functions.len() {
        return Err(refused());
    }
    for expected in &claim.functions {
        let mut matches = functions
            .iter()
            .filter(|f| f["FunctionID"].as_str() == Some(expected.function_id.as_str()));
        let actual = matches.next().ok_or_else(refused)?;
        if matches.next().is_some()
            || actual["Kind"].as_u64() != Some(2)
            || actual["RequireTransaction"].as_bool() != Some(false)
            || actual["Disabled"].as_bool().is_none()
            || actual["Invalid"].as_bool().is_none()
            || (require_published && actual["Disabled"].as_bool() != Some(false))
            || (require_published && actual["Invalid"].as_bool() != Some(false))
            || actual["Private"].as_bool() != Some(false)
            || !actual["Body"].is_string()
        {
            return Err(refused());
        }
        let signature = actual["FunctionSignature"].as_array().ok_or_else(refused)?;
        if !signature.iter().any(|p| {
            p["Response"].as_bool() == Some(false)
                && p["ArgumentName"].as_str() == Some(claim.company_argument.as_str())
                && p["DataType"].as_str() == Some("System.String")
        }) {
            return Err(refused());
        }
        for output in &expected.binary_outputs {
            if !signature.iter().any(|p| {
                p["Response"].as_bool() == Some(true)
                    && p["ArgumentName"].as_str() == Some(output.as_str())
                    && p["DataType"].as_str() == Some("System.Byte[]")
            }) {
                return Err(refused());
            }
        }
        if let Some(resource) = &expected.resource {
            if expected.effect != HelperEffect::OwnedTemporaryState
                || resource.begin_operation.is_empty()
                || resource.status_operation.is_empty()
                || resource.close_operation.is_empty()
                || resource
                    .acknowledge_operation
                    .as_ref()
                    .is_some_and(String::is_empty)
                || !resource.closed_pointer.starts_with('/')
                || !resource.admission_rejected_pointer.starts_with('/')
            {
                return Err(refused());
            }
            for argument in [
                &resource.operation_argument,
                &resource.operation_id_argument,
            ] {
                if argument.is_empty()
                    || !signature.iter().any(|p| {
                        p["Response"].as_bool() == Some(false)
                            && p["ArgumentName"].as_str() == Some(argument.as_str())
                            && p["DataType"].as_str() == Some("System.String")
                    })
                {
                    return Err(refused());
                }
            }
            for argument in &resource.binding_arguments {
                if !signature.iter().any(|p| {
                    p["Response"].as_bool() == Some(false)
                        && p["ArgumentName"].as_str() == Some(argument.as_str())
                }) {
                    return Err(refused());
                }
            }
            for observation in &resource.observations {
                if !observation.pointer.starts_with('/')
                    || !resource.binding_arguments.contains(&observation.argument)
                {
                    return Err(refused());
                }
            }
        } else if expected.effect == HelperEffect::OwnedTemporaryState {
            return Err(refused());
        }
    }
    let requested = claim
        .functions
        .iter()
        .find(|f| f.function_id == function)
        .ok_or_else(refused)?;
    Ok(VerifiedHelper {
        library: claim.library_id.clone(),
        function: function.into(),
        company: company.into(),
        company_argument: claim.company_argument.clone(),
        effect: requested.effect,
    })
}
