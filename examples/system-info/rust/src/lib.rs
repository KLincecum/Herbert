//! Hello from Herbert: configuration, portable state and one explicit capability.
//!
//! The enable switch works. The three demo controls only store example values.
//! System facts always come from the host's curated snapshot, never from ambient
//! access to the user's files, environment variables or credentials.

#[cfg(target_arch = "wasm32")]
mod abi;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CATEGORY: &str = "Example.SystemInfo.Rust";
const ABI: u32 = 1;
const DEMO_NOTE: &str =
    "Demo / example — no effect. This value is saved but does not change system info.";

// Request fields used by this example. The host also supplies scope and paging
// fields. Serde accepts those fields, but this plugin does not need to read them.
#[derive(Deserialize)]
struct Request {
    abi: u32,
    operation: String,
    category: String,
    identity: Option<Value>,
    state: Value,
    response: Option<Value>,
    error: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configuration {
    enabled: bool,
    demo_checkbox: bool,
    demo_choice: String,
    demo_filter: String,
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            enabled: false,
            demo_checkbox: false,
            demo_choice: "alpha".into(),
            demo_filter: String::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Status {
    phase: String,
    run_count: u64,
    last_report: Option<Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableState {
    schema_version: u32,
    config: Configuration,
    status: Status,
}

impl Default for PortableState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            config: Configuration::default(),
            status: Status {
                phase: "disabled".into(),
                run_count: 0,
                last_report: None,
            },
        }
    }
}

pub fn dispatch(bytes: &[u8]) -> Vec<u8> {
    let reply = match serde_json::from_slice(bytes) {
        Ok(request) => handle(request),
        Err(_) => failure(
            "INVALID_PLUGIN_INPUT",
            "The request must match plugin ABI 1.",
        ),
    };
    serde_json::to_vec(&reply).expect("The reply contains only JSON values")
}

fn handle(request: Request) -> Value {
    if request.abi != ABI || (request.operation != "descriptor" && request.category != CATEGORY) {
        return failure(
            "PLUGIN_INCOMPATIBLE",
            "The ABI or collection does not match this plugin.",
        );
    }

    let mut state = match read_state(&request.state["_pluginState"]) {
        Ok(state) => state,
        Err(reply) => return reply,
    };
    match request.operation.as_str() {
        "descriptor" => descriptor(&state.config),
        "configure" => configure(&request.state["values"], state),
        "discover" if state.config.enabled => json!({
            "type": "inventory", "identities": [identity("SystemInfo")],
            "nextCursor": null, "authoritative": true, "diagnostics": []
        }),
        "discover" | "acquire" if !state.config.enabled => failure(
            "COLLECTION_DISABLED",
            "Enable this example before gathering system info.",
        ),
        "acquire" => {
            if request
                .identity
                .as_ref()
                .is_some_and(|value| value != &identity("SystemInfo"))
            {
                return failure(
                    "INVALID_IDENTITY",
                    "Select this example's system-info identity.",
                );
            }
            if let Some(diagnostic) = request.error {
                state.status.phase = "failed".into();
                return save(state, json!({"type": "failure", "diagnostic": diagnostic}));
            }
            if request.response.is_none() {
                state.status.phase = "collecting".into();
                return save(
                    state,
                    json!({
                        "type": "action", "action": {"type": "systemInfo"},
                        "state": {"phase": "systemInfo"}
                    }),
                );
            }
            if request.state["phase"] != "systemInfo" {
                return failure(
                    "INVALID_PLUGIN_STATE",
                    "The system-info continuation is missing.",
                );
            }
            let facts = request.response.unwrap();
            if !facts.is_object() {
                return failure("INVALID_HOST_RESPONSE", "System info must be an object.");
            }
            let Some(run_count) = state.status.run_count.checked_add(1) else {
                return failure(
                    "STATUS_COUNTER_OVERFLOW",
                    "The status counter exceeds its u64 format.",
                );
            };
            let report = json!({"greeting": "Hello from Herbert!", "facts": facts});
            state.status.phase = "complete".into();
            state.status.run_count = run_count;
            state.status.last_report = Some(report.clone());
            save(state, definition("SystemInfo", "system-info", report))
        }
        _ => failure(
            "UNSUPPORTED_OPERATION",
            "This example does not expose that operation.",
        ),
    }
}

fn read_state(value: &Value) -> Result<PortableState, Value> {
    if value.is_null() {
        return Ok(PortableState::default());
    }
    let state: PortableState = serde_json::from_value(value.clone()).map_err(|_| {
        failure(
            "INVALID_PLUGIN_STATE",
            "The portable state does not match this example's schema.",
        )
    })?;
    if state.schema_version != 1 || !["alpha", "beta"].contains(&state.config.demo_choice.as_str())
    {
        return Err(failure(
            "INVALID_PLUGIN_STATE",
            "The portable state version or choice is invalid.",
        ));
    }
    Ok(state)
}

fn configure(values: &Value, mut state: PortableState) -> Value {
    let config: Configuration = match serde_json::from_value(values.clone()) {
        Ok(config) => config,
        Err(_) => {
            return failure(
                "INVALID_PLUGIN_SETTINGS",
                "Supply the four declared setting values.",
            );
        }
    };
    if !["alpha", "beta"].contains(&config.demo_choice.as_str()) {
        return failure(
            "INVALID_PLUGIN_SETTINGS",
            "Choose alpha or beta for the demo choice.",
        );
    }
    state.status.phase = if config.enabled { "ready" } else { "disabled" }.into();
    let reply = definition("PluginConfiguration", "plugin-configuration", json!(config));
    state.config = config;
    save(state, reply)
}

fn descriptor(config: &Configuration) -> Value {
    json!({
        "type": "descriptor", "abi": ABI, "methods": [],
        "category": {
            "id": CATEGORY, "name": "System info (Rust example)",
            "description": "Hello-world example: curated host facts, portable configuration and status.",
            "pluginVersion": env!("CARGO_PKG_VERSION"), "available": config.enabled,
            "qualification": "Teaching example; system facts describe the Herbert host.",
            "roles": ["collector", "local"], "helpers": [],
            "help": [
                {"title": "What this example does", "paragraphs": [
                    "This example greets you and reports Herbert's version, plugin interface version, operating system, processor architecture, pointer size, byte order and available parallelism.",
                    "These facts describe the computer that runs Herbert. The module gathers them during a download and makes no Epicor reads. It does not read credentials, user files or business data."
                ]},
                {"title": "Options and portable state", "paragraphs": [
                    "Enable and select this example to include local system facts in a download. The example starts disabled.",
                    "Demo checkbox, Demo choice and Demo filter are examples only. Their values are saved, but they do not change the system facts or select data.",
                    "Save the options to keep configuration beside the module. Download progress reports execution status. Move the state file with the module to retain its configuration and history."
                ]}
            ],
            "settings": [
                {"kind": "checkbox", "key": "enabled", "label": "Enable this example",
                 "description": "Allow this plugin to gather the host's curated system info.",
                 "default": config.enabled, "demoOnly": false},
                {"kind": "checkbox", "key": "demoCheckbox", "label": "Demo checkbox",
                 "description": DEMO_NOTE, "default": config.demo_checkbox, "demoOnly": true},
                {"kind": "choice", "key": "demoChoice", "label": "Demo choice",
                 "description": DEMO_NOTE, "default": config.demo_choice, "demoOnly": true,
                 "options": [{"label": "Alpha", "value": "alpha"}, {"label": "Beta", "value": "beta"}]},
                {"kind": "filter", "key": "demoFilter", "label": "Demo filter",
                 "description": DEMO_NOTE, "default": config.demo_filter, "demoOnly": true}
            ]
        }
    })
}

fn identity(kind: &str) -> Value {
    json!({"category": CATEGORY, "kind": kind, "scope": {"type": "global"}, "keys": {}})
}

fn definition(kind: &str, format: &str, value: Value) -> Value {
    json!({
        "type": "definition", "identity": identity(kind), "format": format,
        "definition": value, "completeness": "collected", "diagnostics": []
    })
}

// The host writes this opaque value beside the actual .wasm file. The plugin
// never chooses a filesystem path. Moving both files preserves config and status.
fn save(state: PortableState, reply: Value) -> Value {
    json!({"type": "state", "pluginState": state, "reply": reply})
}

fn failure(code: &str, message: &str) -> Value {
    json!({"type": "failure", "diagnostic": {
        "code": code, "message": message, "retryable": false
    }})
}
