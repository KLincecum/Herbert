//! Apply an explicitly incomplete producer operation without changing acquisition.
//! This contains no category, native association, or generated-file policy.
use herbert_contract::{Artifact, Diagnostic, Identity};
use serde_json::json;

pub(crate) fn incomplete(diagnostic: &Diagnostic) -> bool {
    diagnostic.context.get("processingIncomplete") == Some(&json!(true))
}
pub(crate) fn page_owner(
    category: &str,
    owners: &[Identity],
    diagnostic: &Diagnostic,
) -> Result<Option<Identity>, Diagnostic> {
    if !incomplete(diagnostic) {
        return Ok(None);
    }
    diagnostic.identity.as_ref().filter(|owner|owner.category==category&&owners.contains(owner)).cloned().map(Some).ok_or_else(||Diagnostic::new("PLUGIN_OUTPUT_INVALID","An incomplete processing diagnostic requires an exact owner in this validated page."))
}
pub(crate) fn reconcile(artifact: &mut Artifact) {
    if artifact.processing != "failed" && artifact.diagnostics.iter().any(incomplete) {
        artifact.processing = "partial".into();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::{Completeness, Scope};
    use std::collections::BTreeMap;
    fn owner(category: &str, id: &str) -> Identity {
        Identity {
            category: category.into(),
            kind: "Definition".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([("ID".into(), id.into())]),
        }
    }
    fn diagnostic(owner: Identity) -> Diagnostic {
        let mut d = Diagnostic::new(
            "OPTIONAL_READ_FAILED",
            "The explicitly requested optional read failed.",
        );
        d.identity = Some(owner);
        d.context.insert("processingIncomplete".into(), json!(true));
        d
    }
    fn artifact() -> Artifact {
        Artifact {
            identity: owner("C04", "one"),
            classification: "discoveredDefinition".into(),
            acquisition: Completeness::Collected,
            integrity: "hostHashVerified".into(),
            processing: "derived".into(),
            files: vec![],
            dependencies: vec![],
            diagnostics: vec![],
            source_consistency: "nonAtomicUnknown".into(),
        }
    }
    #[test]
    fn explicit_failure_changes_only_processing_and_diagnostics() {
        let mut a = artifact();
        let before = a.clone();
        a.diagnostics.push(diagnostic(a.identity.clone()));
        reconcile(&mut a);
        assert_eq!(a.processing, "partial");
        a.processing = before.processing.clone();
        a.diagnostics.clear();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
    #[test]
    fn marked_failure_requires_exact_current_page_owner() {
        let selected = owner("C04", "one");
        assert_eq!(
            page_owner("C04", &[selected.clone()], &diagnostic(selected.clone())).unwrap(),
            Some(selected.clone())
        );
        for other in [owner("C04", "two"), owner("C06", "one")] {
            assert!(page_owner("C04", &[selected.clone()], &diagnostic(other)).is_err());
        }
        let mut d = diagnostic(selected);
        d.identity = None;
        assert!(page_owner("C04", &[], &d).is_err());
    }
    #[test]
    fn ordinary_diagnostics_and_verified_absence_do_not_force_partial() {
        let mut a = artifact();
        a.diagnostics
            .push(Diagnostic::new("INFORMATION", "A normal observation."));
        reconcile(&mut a);
        assert_eq!(a.processing, "derived");
        assert_eq!(page_owner("C04", &[], &a.diagnostics[0]).unwrap(), None);
        a.acquisition = Completeness::VerifiedAbsent;
        reconcile(&mut a);
        assert_eq!(a.processing, "derived");
    }
    #[test]
    fn unresolved_failure_survives_reprocessing_until_producer_clears_it() {
        let mut a = artifact();
        a.diagnostics.push(diagnostic(a.identity.clone()));
        for _ in 0..2 {
            a.processing = "derived".into();
            reconcile(&mut a);
            assert_eq!(a.processing, "partial");
        }
        a.processing = "failed".into();
        reconcile(&mut a);
        assert_eq!(a.processing, "failed");
        a.diagnostics.clear();
        a.processing = "derived".into();
        reconcile(&mut a);
        assert_eq!(a.processing, "derived");
    }
}
