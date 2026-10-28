//! Read personal ownership from source metadata. Never substitute the initiating caller.
use herbert_contract::{Diagnostic, Scope};
use serde_json::Value;

pub fn scope(
    row: &Value,
    selected_company: &str,
    owner_field: Option<&str>,
    owner_required: bool,
) -> Result<Scope, Diagnostic> {
    let actual_company = match row.get("Company") {
        None => None,
        Some(Value::String(company)) => Some(company.as_str()),
        _ => {
            return Err(Diagnostic::new(
                "SCOPE_UNVERIFIED",
                "The native Company field is not text.",
            ));
        }
    };
    if actual_company.is_some_and(|c| !c.is_empty() && c != selected_company) {
        return Err(Diagnostic::new(
            "SCOPE_MISMATCH",
            "The native definition is outside the selected company.",
        ));
    }
    if let Some(field) = owner_field {
        let owner = row.get(field).and_then(Value::as_str).ok_or_else(|| {
            Diagnostic::new(
                "OWNER_UNVERIFIED",
                "The declared native owner field is absent or malformed.",
            )
        })?;
        if !owner.is_empty() {
            return Ok(Scope::Personal {
                company: actual_company.unwrap_or(selected_company).into(),
                owner: owner.into(),
            });
        }
        if owner_required {
            return Err(Diagnostic::new(
                "OWNER_UNVERIFIED",
                "The native personalization row does not identify its owner.",
            ));
        }
    } else if owner_required {
        return Err(Diagnostic::new(
            "OWNER_UNVERIFIED",
            "A personal definition has no declared native owner field.",
        ));
    }
    Ok(match actual_company {
        Some(company) if !company.is_empty() => Scope::Company {
            company: company.into(),
        },
        _ => Scope::Global,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn personalization_uses_key2_and_retains_blank_global_company() {
        let value = json!({"Company":"","Key2":"native-owner","Key3":"WIP"});
        assert_eq!(
            scope(&value, "EXAMPLE", Some("Key2"), true).unwrap(),
            Scope::Personal {
                company: "".into(),
                owner: "native-owner".into()
            }
        );
        assert!(
            scope(
                &json!({"Company":"","Key2":""}),
                "EXAMPLE",
                Some("Key2"),
                true
            )
            .is_err()
        );
    }
    #[test]
    fn shared_named_search_and_foreign_company_are_distinct() {
        assert_eq!(
            scope(
                &json!({"Company":"EXAMPLE","UserID":""}),
                "EXAMPLE",
                Some("UserID"),
                false
            )
            .unwrap(),
            Scope::Company {
                company: "EXAMPLE".into()
            }
        );
        assert!(
            scope(
                &json!({"Company":"OTHER","UserID":"native-owner"}),
                "EXAMPLE",
                Some("UserID"),
                false
            )
            .is_err()
        );
        assert!(
            scope(
                &json!({"Company":"EXAMPLE"}),
                "EXAMPLE",
                Some("UserID"),
                false
            )
            .is_err()
        );
    }
}
