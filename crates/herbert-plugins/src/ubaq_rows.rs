//! Read native UBAQ methods in shared pages and retain their exact workflow text.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const SERVICE: &str = "Ice.BO.BpMethodSvc";

pub fn next(mut state: Value, page_size: Option<u32>) -> GuestReply {
    let page = state["bpmPage"].as_u64().unwrap_or(1);
    state["phase"] = json!("bpmRows");
    state["_progress"] = json!({
        "request":format!("Downloading updatable-query methods and widgets - page {page}"),
        "response":"Update methods received; preserving widgets and building query files"
    });
    GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: SERVICE.into(), method: "GetRowsEx".into(), export: false,
            // GetRows alone defaults to BO methods. GetRowsEx selects the native
            // DQ source. Shared pages avoid reading each query's methods again.
            arguments: json!({"source":"DQ","whereClauseBpMethod":"",
                "whereClauseBpDirective":"","pageSize":page_size,"absolutePage":page}),
        }, state,
    }
}

pub fn append(state: &mut Value, response: &Value) -> Result<bool, Diagnostic> {
    let invalid = || Diagnostic::new("UBAQ_METHODS_INVALID", "The native update-method page is incomplete.");
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        let rows = response["returnObj"][table].as_array().ok_or_else(invalid)?;
        if !state["bpm"][table].is_array() { state["bpm"][table] = json!([]); }
        state["bpm"][table].as_array_mut().unwrap().extend(rows.iter().cloned());
    }
    let more = response["parameters"]["morePages"].as_bool().ok_or_else(invalid)?;
    if more && response["returnObj"]["BpMethod"].as_array().unwrap().is_empty() {
        return Err(invalid());
    }
    state["bpmPage"] = json!(state["bpmPage"].as_u64().unwrap_or(1) + 1);
    Ok(more)
}

/// TypeInfo retains the authored query name even when CLR method names escape
/// punctuation. Never infer query ownership from a prefix or a display name.
pub fn select(dataset: &Value, company: &str, query: &str) -> Result<Value, Diagnostic> {
    let mut methods = BTreeSet::new();
    for argument in dataset["BpArgument"].as_array().into_iter().flatten() {
        let Some(info) = argument["TypeInfo"].as_str()
            .and_then(|text| serde_json::from_str::<Value>(text).ok()) else { continue; };
        if info["company"].as_str().is_some_and(|v| v.eq_ignore_ascii_case(company))
            && info["query"].as_str().is_some_and(|v| v.eq_ignore_ascii_case(query)) {
            if let Some(method) = argument["BpMethodCode"].as_str() {
                methods.insert(method.to_owned());
            }
        }
    }
    // An updatable query can have no methods. Keep an empty method dataset.
    // Read failures and invalid pages are handled before this selection step.
    let mut selected = json!({});
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        selected[table] = json!(dataset[table].as_array().into_iter().flatten()
            .filter(|row| row["Source"] == "DQ"
                && row["BpMethodCode"].as_str().is_some_and(|code| methods.contains(code)))
            .cloned().collect::<Vec<_>>());
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_without_methods_is_valid_but_incomplete_reads_fail() {
        let empty = json!({"BpMethod":[], "BpArgument":[], "BpDirective":[]});
        assert_eq!(select(&empty, "C", "NoMethods").unwrap(), empty);
        let mut state = json!({});
        assert!(append(&mut state, &json!({
            "returnObj": {"BpMethod":[], "BpArgument":[]},
            "parameters": {"morePages": false}
        })).is_err());
    }

    #[test]
    fn authored_identity_selects_escaped_names_and_preserves_widget_body() {
        let body = "<DirectiveDefinition2><Widget Text=\"a &amp; b\" /></DirectiveDefinition2>";
        let data = json!({"BpMethod":[{"Source":"DQ","BpMethodCode":"Ice.C/Q_with_dash.GetList"}],
            "BpArgument":[{"Source":"DQ","BpMethodCode":"Ice.C/Q_with_dash.GetList",
                "TypeInfo":"{\"company\":\"C\",\"query\":\"Q-with-dash\"}"}],
            "BpDirective":[{"Source":"DQ","BpMethodCode":"Ice.C/Q_with_dash.GetList","Body":body},
                {"Source":"DQ","BpMethodCode":"Ice.C/Q_with_dash_extra.GetList","Body":"other"}]});
        let selected = select(&data, "C", "Q-with-dash").unwrap();
        assert_eq!(selected["BpDirective"].as_array().unwrap().len(), 1);
        assert_eq!(selected["BpDirective"][0]["Body"], body);
        let unmatched = select(&data, "Other", "Q-with-dash").unwrap();
        for table in ["BpMethod", "BpArgument", "BpDirective"] {
            assert_eq!(unmatched[table], json!([]));
        }
    }
}
