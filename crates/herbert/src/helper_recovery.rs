//! Validate restoration of the exact original helper for a retained transfer.
//! Native absence, locking, source discovery and lifecycle writes remain with Host.
use crate::{helper_libraries::LibraryLease, helpers::HelperTransfer};
use herbert_contract::{Diagnostic, HelperContract};
use serde_json::Value;

pub(crate) struct RecoveryScope<'a> {
    pub run_id: &'a str,
    pub endpoint: &'a str,
    pub company: &'a str,
    pub caller: &'a str,
}

fn refused(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn same<T: serde::Serialize>(a: &T, b: &T) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

/// Validate both retained ownership records before selecting any source or writing.
/// Obtain the lease from the exact current LeaseFile. Do not call LeaseFile::open
/// with a missing path, because that creates a new identity.
pub(crate) fn validate_binding(
    lease: &LibraryLease,
    journal_lease: &LibraryLease,
    transfer: &HelperTransfer,
    scope: &RecoveryScope<'_>,
) -> Result<(), Diagnostic> {
    let id = uuid::Uuid::parse_str(&lease.instance.global_id).map_err(|_| {
        refused(
            "HELPER_RECOVERY_IDENTITY_INVALID",
            "The retained helper UUID is invalid.",
        )
    })?;
    let immutable_match = lease.schema_version == 1
        && journal_lease.schema_version == 1
        && lease.holder == scope.run_id
        && lease.endpoint == scope.endpoint
        && lease.company == scope.company
        && lease.caller == scope.caller
        && lease.holder == journal_lease.holder
        && lease.endpoint == journal_lease.endpoint
        && lease.company == journal_lease.company
        && lease.caller == journal_lease.caller
        && lease.template_id == journal_lease.template_id
        && lease.owner_marker == journal_lease.owner_marker
        && same(&lease.instance, &journal_lease.instance)
        && same(&lease.instance, &transfer.claim)
        && !id.is_nil()
        && id.to_string() == lease.instance.global_id
        && lease.instance.library_id == format!("H{}", &id.simple().to_string()[..29])
        && lease.owner_marker == format!("Herbert temporary helper; lease {id}")
        && (lease.phase == "ready"
            || journal_lease.phase == "ready"
            || lease.transitions.iter().any(|phase| phase == "ready"))
        && matches!(
            lease.phase.as_str(),
            "ready"
                | "installed"
                | "enablePending"
                | "enabled"
                | "compilationPending"
                | "compiled"
                | "activationPending"
                | "cleanupPending"
                | "removed"
                | "restorePending"
                | "importPending"
        )
        && matches!(
            transfer.phase.as_str(),
            "admitted" | "open" | "cleanupPending" | "closedPendingAcknowledgement"
        );
    if !immutable_match {
        return Err(refused(
            "HELPER_RECOVERY_OWNER_MISMATCH",
            "The retained run, transfer and helper generation do not prove the same ownership. Preserve their records and server staging.",
        ));
    }
    let resource = transfer
        .claim
        .functions
        .iter()
        .find(|f| f.function_id == transfer.function)
        .and_then(|f| f.resource.as_ref())
        .ok_or_else(|| {
            refused(
                "HELPER_RECOVERY_CONTRACT_MISSING",
                "The retained cleanup resource contract is missing.",
            )
        })?;
    let operation = uuid::Uuid::parse_str(&transfer.operation_id).map_err(|_| {
        refused(
            "HELPER_RECOVERY_IDENTITY_INVALID",
            "The retained transfer UUID is invalid.",
        )
    })?;
    let expected_cleanup = if transfer.phase == "closedPendingAcknowledgement" {
        resource.acknowledge_operation.as_deref().ok_or_else(|| {
            refused(
                "HELPER_RECOVERY_CONTRACT_MISSING",
                "The retained closure has no acknowledgement contract.",
            )
        })?
    } else {
        resource.close_operation.as_str()
    };
    if operation.is_nil()
        || operation.to_string() != transfer.operation_id
        || transfer
            .close_arguments
            .get(&resource.operation_id_argument)
            .and_then(Value::as_str)
            != Some(transfer.operation_id.as_str())
        || transfer
            .close_arguments
            .get(&resource.operation_argument)
            .and_then(Value::as_str)
            != Some(expected_cleanup)
        || transfer
            .close_arguments
            .get(&lease.instance.company_argument)
            .and_then(Value::as_str)
            != Some(scope.company)
        || resource
            .binding_arguments
            .iter()
            .any(|name| transfer.close_arguments.get(name).is_none())
    {
        return Err(refused(
            "HELPER_RECOVERY_BINDING_INVALID",
            "The retained operation, cleanup stage or binding arguments are incomplete. Preserve staging; do not invent replacement arguments.",
        ));
    }
    Ok(())
}

/// Call with bytes returned by the EXISTING LeaseFile::package method, preserving
/// LibraryID, GlobalID, owner marker and mapping. Compare with original admitted bytes.
pub(crate) fn verify_bound_bytes(
    lease: &LibraryLease,
    bytes: &[u8],
    native_absence_confirmed: bool,
) -> Result<(), Diagnostic> {
    if !native_absence_confirmed {
        return Err(refused(
            "HELPER_RECOVERY_ABSENCE_REQUIRED",
            "A successful exact Epicor ID read must prove absence before restoration.",
        ));
    }
    let json = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).ok_or_else(|| {
        refused(
            "HELPER_RECOVERY_PACKAGE_MISMATCH",
            "The original helper package framing is missing.",
        )
    })?;
    let package: Value = serde_json::from_slice(json).map_err(|_| {
        refused(
            "HELPER_RECOVERY_PACKAGE_MISMATCH",
            "The helper package is invalid.",
        )
    })?;
    if package["Library"]["Owner"].as_str() != Some(lease.caller.as_str())
        || package["Library"]["OwnedByCompany"].as_str() != Some(lease.company.as_str())
        || package["Library"]["Notes"].as_str() != Some(lease.owner_marker.as_str())
    {
        return Err(refused(
            "HELPER_RECOVERY_OWNER_MISMATCH",
            "The rebound package changed retained ownership.",
        ));
    }
    for function in &lease.instance.functions {
        crate::helpers::verify_package(
            &lease.instance,
            &function.function_id,
            &package,
            &lease.company,
            false,
        )?;
    }
    Ok(())
}

pub(crate) fn matches_template(template: &HelperContract, instance: &HelperContract) -> bool {
    let mut bound = template.clone();
    bound.library_id = instance.library_id.clone();
    bound.global_id = instance.global_id.clone();
    bound.installation = None;
    same(&bound, instance)
}

/// Synthetic package for local recovery tests. No native implementation is included.
#[cfg(test)]
pub(crate) fn recovery_test_contract() -> HelperContract {
    use serde_json::json;
    let bindings = ["iFolderIsNull", "iFolder", "iAssemblyIsNull", "iAssembly",
        "iVersionIsNull", "iVersion", "iExpectedFingerprint", "iStorageInstanceId"];
    let signature: Vec<Value> = ["iCompany", "iOperation", "iOperationId"]
        .into_iter().chain(bindings).map(|name| json!({
            "ArgumentName": name, "Response": false,
            "DataType": if name.ends_with("IsNull") { "System.Boolean" } else { "System.String" }
        })).collect();
    let package = json!({
        "Mode": "Backup", "LibraryId": "FixtureRecovery", "Library": {
            "GlobalID": "13713962-c6d7-485c-91fd-df00c9da9398",
            "DirectDBAccess": 0, "AllowCustomCodeWidgets": false,
            "Disabled": false, "Published": false, "Private": false,
            "LibraryMappings": [], "LibraryReferences": [],
            "Functions": [{"FunctionID": "ReadChunk", "Kind": 2,
                "RequireTransaction": false, "Disabled": false, "Invalid": false,
                "Private": false, "Body": "{}", "FunctionSignature": signature}]
        }
    });
    serde_json::from_value(json!({
        "libraryId": "FixtureRecovery", "globalId": "13713962-c6d7-485c-91fd-df00c9da9398",
        "companyArgument": "iCompany", "functions": [{
            "functionId": "ReadChunk", "binaryOutputs": [], "effect": "ownedTemporaryState",
            "resource": {"operationArgument": "iOperation", "operationIdArgument": "iOperationId",
                "beginOperation": "Begin", "statusOperation": "Status", "closeOperation": "Close",
                "acknowledgeOperation": "Acknowledge", "closedPointer": "/oDS/Transfer/0/Closed",
                "admissionRejectedPointer": "/oAdmissionRejected", "bindingArguments": bindings,
                "observations": [{"argument": "iStorageInstanceId", "pointer": "/oDS/Transfer/0/StorageInstanceId"}]}
        }],
        "installation": {"supportsInstances": true,
            "packages": [{"epicorVersion": "5.2.100", "package": package}]}
    })).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage;
    use herbert_contract::{Identity, Scope};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn fixture() -> (LibraryLease, HelperTransfer, HelperContract, Vec<u8>) {
        let template = crate::helper_recovery::recovery_test_contract();
        let id = uuid::Uuid::parse_str("cb250c1b-4606-4ed0-a6fb-215e7088996d").unwrap();
        let mut instance = template.clone();
        instance.library_id = format!("H{}", &id.simple().to_string()[..29]);
        instance.global_id = id.to_string();
        instance.installation = None;
        let lease = LibraryLease {
            schema_version: 1,
            holder: "5b2b08a2-a540-4fb2-a142-93cac4cdf843".into(),
            endpoint: "https://fixture.invalid/".into(),
            company: "FIXTURE".into(),
            caller: "fixture-caller".into(),
            template_id: template.library_id.clone(),
            plugin_hash: "exact-module-content-hash".into(),
            instance: instance.clone(),
            owner_marker: format!("Herbert temporary helper; lease {id}"),
            phase: "ready".into(),
            transitions: vec![
                "reserved".into(),
                "importPending".into(),
                "installed".into(),
                "ready".into(),
            ],
            package_sha256: None,
            frozen_package: None,
            authorization: None,
        };
        let package = template.installation.as_ref().unwrap().packages[0]
            .package
            .clone();
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("helper-libraries");
        std::fs::create_dir(&directory).unwrap();
        let key =
            storage::sha256(&serde_json::to_vec(&(&lease.holder, &lease.template_id)).unwrap());
        std::fs::write(
            directory.join(format!("{key}.json")),
            serde_json::to_vec(&lease).unwrap(),
        )
        .unwrap();
        let file = crate::helper_libraries::LeaseFile::existing(
            root.path(),
            &lease.holder,
            &lease.template_id,
        )
        .unwrap();
        let (bytes, _) = file.package(&package).unwrap();

        let transfer = HelperTransfer {
            operation_id: "e55fe5e9-e334-4314-9097-9fac04406f21".into(),
            identity: Identity {
                category: "fixtureResource".into(),
                kind: "FixtureResource".into(),
                scope: Scope::Global,
                keys: BTreeMap::new(),
            },
            claim: instance,
            function: "ReadChunk".into(),
            close_arguments: json!({
                "iOperation":"Close", "iOperationId":"e55fe5e9-e334-4314-9097-9fac04406f21",
                "iCompany":"FIXTURE", "iFolderIsNull":false, "iFolder":"fixture-folder",
                "iAssemblyIsNull":false, "iAssembly":"fixture-assembly",
                "iVersionIsNull":true, "iVersion":"",
                "iExpectedFingerprint":"original-snapshot-fingerprint",
                "iStorageInstanceId":"18941a9bd4f044e3bc7f160cc31f47d3",
                "iAllowCreate":false, "iPayload":"", "iOffset":0, "iRequestedBytes":0
            }),
            phase: "open".into(),
            plugin_hash: lease.plugin_hash.clone(),
            dispatch_count: 1,
        };
        (lease, transfer, template, bytes)
    }
    fn scope(lease: &LibraryLease) -> RecoveryScope<'_> {
        RecoveryScope {
            run_id: &lease.holder,
            endpoint: &lease.endpoint,
            company: &lease.company,
            caller: &lease.caller,
        }
    }
    #[test]
    fn ready_legacy_lease_without_transition_history_keeps_exact_source_admission() {
        let (mut lease, transfer, _, bytes) = fixture();
        lease.transitions.clear();
        validate_binding(&lease, &lease, &transfer, &scope(&lease)).unwrap();
        verify_bound_bytes(&lease, &bytes, true).unwrap();
    }
    #[test]
    fn restart_restores_exact_generation_bytes_and_leaves_transfer_binding_untouched() {
        let (lease, transfer, _template, bytes) = fixture();
        let before = serde_json::to_vec(&transfer).unwrap();
        let saved: LibraryLease =
            serde_json::from_slice(&serde_json::to_vec(&lease).unwrap()).unwrap();
        let retained: HelperTransfer = serde_json::from_slice(&before).unwrap();
        validate_binding(&saved, &lease, &retained, &scope(&lease)).unwrap();
        verify_bound_bytes(&saved, &bytes, true).unwrap();
        assert_eq!(serde_json::to_vec(&retained).unwrap(), before);
        assert_eq!(saved.instance.library_id, "Hcb250c1b46064ed0a6fb215e70889");
        assert_eq!(
            saved.instance.global_id,
            "cb250c1b-4606-4ed0-a6fb-215e7088996d"
        );
        assert_eq!(
            retained.operation_id,
            "e55fe5e9-e334-4314-9097-9fac04406f21"
        );
        assert_eq!(
            retained.close_arguments["iStorageInstanceId"],
            "18941a9bd4f044e3bc7f160cc31f47d3"
        );
        assert_eq!(
            retained.close_arguments["iExpectedFingerprint"],
            "original-snapshot-fingerprint"
        );
        assert_eq!(retained.close_arguments["iAllowCreate"], false);
    }
    #[test]
    fn absence_and_native_identity_are_required_without_code_hashes() {
        let (lease, _, _, bytes) = fixture();
        assert_eq!(
            verify_bound_bytes(&lease, &bytes, false).unwrap_err().code,
            "HELPER_RECOVERY_ABSENCE_REQUIRED"
        );
        let mut changed = bytes.clone();
        changed.push(b' ');
        verify_bound_bytes(&lease, &changed, true).unwrap();
        let mut foreign: Value = serde_json::from_slice(&bytes[3..]).unwrap();
        foreign["Library"]["Owner"] = json!("another caller");
        let mut bad = vec![0xef, 0xbb, 0xbf];
        bad.extend(serde_json::to_vec(&foreign).unwrap());
        assert_eq!(
            verify_bound_bytes(&lease, &bad, true).unwrap_err().code,
            "HELPER_RECOVERY_OWNER_MISMATCH"
        );
    }
    #[test]
    fn another_caller_generation_or_journal_cannot_claim_the_staging() {
        let (lease, transfer, _, _) = fixture();
        for which in 0..6 {
            let mut changed = lease.clone();
            match which {
                0 => changed.caller = "other-caller".into(),
                1 => changed.holder = uuid::Uuid::new_v4().to_string(),
                2 => changed.endpoint = "https://other.invalid/".into(),
                3 => changed.instance.global_id = uuid::Uuid::new_v4().to_string(),
                4 => changed.owner_marker = "foreign owner".into(),
                _ => changed.template_id = "different template".into(),
            }
            assert_eq!(
                validate_binding(&changed, &lease, &transfer, &scope(&lease))
                    .unwrap_err()
                    .code,
                "HELPER_RECOVERY_OWNER_MISMATCH"
            );
        }
    }
    #[test]
    fn changed_resource_contract_is_not_a_recovery_match() {
        let (lease, _, template, _) = fixture();
        assert!(matches_template(&template, &lease.instance));
        let mut changed = template.clone();
        changed
            .functions
            .iter_mut()
            .find(|f| f.function_id == "ReadChunk")
            .unwrap()
            .resource
            .as_mut()
            .unwrap()
            .binding_arguments
            .push("newBinding".into());
        assert!(!matches_template(&changed, &lease.instance));
    }
    #[test]
    fn acknowledgement_and_unknown_admission_storage_identity_remain_distinct() {
        let (lease, mut transfer, _, _) = fixture();
        transfer.phase = "closedPendingAcknowledgement".into();
        assert_eq!(
            validate_binding(&lease, &lease, &transfer, &scope(&lease))
                .unwrap_err()
                .code,
            "HELPER_RECOVERY_BINDING_INVALID"
        );
        transfer.close_arguments["iOperation"] = json!("Acknowledge");
        validate_binding(&lease, &lease, &transfer, &scope(&lease)).unwrap();
        transfer.phase = "admitted".into();
        transfer.close_arguments["iOperation"] = json!("Close");
        transfer.close_arguments["iStorageInstanceId"] = json!("");
        // An admission response may have been lost. This does not invent a storage
        // identity or prove closure. The native helper must still validate its owner.
        validate_binding(&lease, &lease, &transfer, &scope(&lease)).unwrap();
        transfer
            .close_arguments
            .as_object_mut()
            .unwrap()
            .remove("iExpectedFingerprint");
        assert_eq!(
            validate_binding(&lease, &lease, &transfer, &scope(&lease))
                .unwrap_err()
                .code,
            "HELPER_RECOVERY_BINDING_INVALID"
        );
    }
    #[test]
    fn interrupted_restore_keeps_immutable_ownership_despite_checkpoint_phase_lag() {
        let (mut lease, transfer, _, bytes) = fixture();
        let journal = lease.clone();
        lease.phase = "importPending".into();
        lease
            .transitions
            .extend(["restorePending".into(), "importPending".into()]);
        validate_binding(&lease, &journal, &transfer, &scope(&journal)).unwrap();
        verify_bound_bytes(&lease, &bytes, true).unwrap();
        let mut terminal = transfer.clone();
        terminal.phase = "closed".into();
        assert!(validate_binding(&lease, &journal, &terminal, &scope(&journal)).is_err());
    }
}
