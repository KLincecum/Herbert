//! What to do next after an error, by diagnostic code.
//! The message says what happened; the next step says what the user can do. Shown after
//! the GUI notice, in the run issue panel and as the MCP error text (`content[1]`).
//! Entries are exact where the action is known; data-shape failures share one sentence;
//! everything else falls back on `retryable`. A message that already gives a step gets no
//! second one in a notice or MCP text, so the two can never disagree.
use herbert_contract::Diagnostic;

const RETRY_THEN_REPORT: &str =
    "Try the download again. If the same error appears, save diagnostics and report the problem.";
const MAY_CLEAR: &str =
    "This may clear on its own. Try again in a moment; if it keeps happening, save diagnostics and report the problem.";
const REPORT: &str = "If this keeps happening, save diagnostics and report the problem.";

/// Known codes whose action Herbert can name.
fn exact(code: &str) -> Option<&'static str> {
    Some(match code {
        "CONNECTION_REQUIRED" | "SESSION_REQUIRED" | "ADMISSION_REQUIRED" => "Connect to Epicor, then try again.",
        "CREDENTIALS_REQUIRED" | "API_KEY_REQUIRED" => "Enter the missing credentials in the connection profile, then connect again.",
        "PROFILE_REQUIRED" | "PROFILE_NOT_FOUND" => "Choose a saved connection profile, or create one, then connect again.",
        "AUTHENTICATION_FAILED" | "ACCESS_DENIED" => "Check the user name, password and API key, and that this Epicor account may read this item, then try again.",
        "SECURE_STORE_UNAVAILABLE" => "Enter the credentials for this session instead, or save them again while signed in to the same Windows account.",
        "OUTPUT_UNAVAILABLE" | "OUTPUT_WRITE_FAILED" | "MANIFEST_UNAVAILABLE" => "Check that the download folder still exists, that you can write to it and that its drive has free space, then start the download again.",
        "SETTINGS_UNAVAILABLE" | "PROFILE_STORAGE_UNAVAILABLE" | "COLLECTION_PREFERENCE_UNAVAILABLE" => "Check that Herbert can read and write its settings folder (security software can block it), then restart Herbert.",
        "INVALID_SETTING" | "INVALID_CONCURRENCY" | "INVALID_SERVER_CONCURRENCY" | "INVALID_DECOMPILER_WORKERS" | "PAGE_SIZE_REQUIRED" => "Correct the setting this message names, then try again.",
        "SERVER_CHANGE_PREVIEW_REQUIRED" | "SERVER_CHANGE_AUTHORIZATION_REQUIRED" => "Preview the download again and review its server changes before starting it.",
        "RUN_ACTIVE" => "Wait for the current download to finish, or cancel it, then try again.",
        "HOST_CLOSED" | "SCHEDULER_CLOSED" => "Restart Herbert, then try again.",
        "PLUGIN_FOLDER_UNAVAILABLE" | "PLUGIN_UNAVAILABLE" | "PLUGIN_RUNTIME_UNAVAILABLE" => "Check the plugins folder chosen in Herbert. Reinstall the plugins from this Herbert release if the folder is damaged.",
        "HELPER_COMPILATION_FAILED" => "Check that this Epicor version is supported by this Herbert release. To download without the server helper, choose the Direct collection method.",
        // Run issue sentences, kept verbatim from gui_run_issues.rs.
        "KINETIC_PARENT_NOT_FOUND" => "Check the named parent layer before restoring this package. Retrying only helps if the parent becomes available.",
        "KINETIC_RESOURCE_PATH_UNVERIFIED" => "Review the named source file and reference. Check the saved output for the original definition. The referenced resource was not verified.",
        "KINETIC_PACKAGE_INVALID" => "Review the saved recovery files and the package explanation. Do not treat recovery JSON as an importable ZIP.",
        "KINETIC_PACKAGE_RECOVERY_ONLY" => "Keep the saved custom recovery files. This collector did not produce a standalone application package for this group; it does not mean the server read failed.",
        "DIAGNOSTIC_LOG_WRITE_FAILED" => "Check destination access and preserve the run journal for diagnosis.",
        _ => return None,
    })
}

/// Epicor returned data, or saved progress held data, in a shape Herbert does not accept.
/// One retry can succeed (a page changed mid-read); a repeat needs a report.
fn data_shape(code: &str) -> bool {
    ["_INVALID", "_UNVERIFIED", "_MISMATCH", "_MALFORMED", "_UNRESOLVED", "_MISSING", "_DUPLICATE"]
        .iter()
        .any(|suffix| code.ends_with(suffix))
}

/// Sentence-initial verbs that mean the message already tells the user what to do. Words that
/// also start descriptive sentences ("Run state…", "Report layout…") are deliberately absent.
const VERBS: &[&str] = &[
    "add", "allow", "ask", "cancel", "check", "choose", "close", "confirm", "connect", "correct", "create",
    "disconnect", "download", "enable", "enter", "fix", "grant", "install", "keep", "make", "move", "open",
    "preview", "provide", "reconnect", "reduce", "reinstall", "remove", "rename", "reopen", "restart", "resume",
    "retry", "review", "save", "select", "sign", "specify", "start", "supply", "try", "update", "use", "wait",
];

/// True when some sentence of the message is an instruction ("Check …", "If …, retry …").
pub(crate) fn has_next_step(message: &str) -> bool {
    message.split_inclusive(['.', '!', '?', ';']).any(|sentence| {
        let sentence = sentence.trim();
        let clause = match sentence.strip_prefix("If ").or_else(|| sentence.strip_prefix("When ")) {
            Some(rest) => rest.split_once(", ").map_or("", |(_, then)| then),
            None => sentence,
        };
        let first = clause.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("").to_ascii_lowercase();
        VERBS.contains(&first.as_str()) || clause.contains(", then ")
    })
}

/// The run issue panel always shows a step: the known one, the data-shape one, or the
/// panel's own general advice.
pub(crate) fn for_issue(code: &str, retryable: bool) -> &'static str {
    exact(code).unwrap_or(if data_shape(code) {
        RETRY_THEN_REPORT
    } else if retryable {
        "This problem may clear on its own. Fix the cause described above if you can, then start the download again."
    } else {
        "Review the explanation and saved files. Copy these details when reporting the problem."
    })
}

/// The step to show after a notice or in MCP error text, or None when the message gives one.
pub(crate) fn for_diagnostic(error: &Diagnostic) -> Option<&'static str> {
    if has_next_step(&error.message) {
        return None;
    }
    Some(exact(&error.code).unwrap_or(if data_shape(&error.code) {
        RETRY_THEN_REPORT
    } else if error.retryable {
        MAY_CLEAR
    } else {
        REPORT
    }))
}

/// MCP `content[1]`: the message and, when it gives none, the next step, as plain text.
/// `structuredContent` and `content[0]` are unchanged.
pub(crate) fn mcp_text(error: &Diagnostic) -> String {
    match for_diagnostic(error) {
        Some(step) if step.contains("save diagnostics") => format!(
            "{}\nNext step: {step} The diagnostics_export tool saves the packet.",
            error.message
        ),
        Some(step) => format!("{}\nNext step: {step}", error.message),
        None => error.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_that_already_instruct_get_no_second_step() {
        for message in [
            "Choose an unused destination and try again.",
            "Herbert could not save. Check that the folder is writable, then retry.",
            "The page was rejected. If it fails again, download the whole category instead.",
            "Epicor refused the import; retry after checking the BAQ.",
            "The only held connection is a connection test. Disconnect to retry the sign-out, then connect.",
        ] {
            assert!(has_next_step(message), "{message}");
        }
        for message in [
            "No active connection exists.",
            "Run state is unavailable.",
            "Report layout Main could not be read.",
            "Epicor returned a definition in a form Herbert does not recognize, so it was not saved.",
        ] {
            assert!(!has_next_step(message), "{message}");
        }
    }
    #[test]
    fn every_diagnostic_without_a_step_gets_one_and_known_codes_get_their_own() {
        let missing = Diagnostic::new("CONNECTION_REQUIRED", "No active connection exists.");
        assert_eq!(for_diagnostic(&missing), Some("Connect to Epicor, then try again."));
        let instructed = Diagnostic::new("CONNECTION_REQUIRED", "The only held connection is a connection test. Disconnect to retry the sign-out, then connect.");
        assert_eq!(for_diagnostic(&instructed), None, "never contradict the message");
        let shape = Diagnostic::new("AUTOMATION_GRAPH_INVALID", "Epicor returned an automation graph Herbert does not recognize.");
        assert_eq!(for_diagnostic(&shape), Some(RETRY_THEN_REPORT));
        let mut busy = Diagnostic::new("SOMETHING_BUSY", "The server was busy.");
        assert_eq!(for_diagnostic(&busy), Some(REPORT));
        busy.retryable = true;
        assert_eq!(for_diagnostic(&busy), Some(MAY_CLEAR));
        for step in [RETRY_THEN_REPORT, MAY_CLEAR, REPORT, for_issue("KINETIC_PARENT_NOT_FOUND", false)] {
            assert!(has_next_step(step), "each step is itself an instruction: {step}");
        }
    }
    #[test]
    fn mcp_text_names_the_export_tool_only_when_it_asks_for_diagnostics() {
        let shape = Diagnostic::new("BAQ_PAGE_INVALID", "Epicor returned a BAQ page Herbert does not recognize.");
        assert_eq!(mcp_text(&shape), format!("{}\nNext step: {RETRY_THEN_REPORT} The diagnostics_export tool saves the packet.", shape.message));
        let missing = Diagnostic::new("CONNECTION_REQUIRED", "No active connection exists.");
        assert_eq!(mcp_text(&missing), "No active connection exists.\nNext step: Connect to Epicor, then try again.");
        let instructed = Diagnostic::new("INVALID_SETTING", "Retry attempts must be a whole number of 0 or more. Correct it, then save.");
        assert_eq!(mcp_text(&instructed), instructed.message);
    }
}
