//! CLI and MCP framing over one versioned application command surface.
use crate::application::{Command, Host, OPERATIONS, Reply, operation_input_schema, operation_output_schema};
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub const MCP_VERSION: &str = "2025-11-25";
pub async fn cli(host: Arc<Host>) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = zeroize::Zeroizing::new(line);
        let reply = match serde_json::from_str::<Command>(&line) {
            Ok(command) => host.execute(command).await,
            Err(_) => Reply {
                schema_version: 1,
                ok: false,
                result: None,
                error: Some(Diagnostic::new(
                    "INVALID_INPUT",
                    "The command is malformed or does not match the versioned command schema.",
                )),
            },
        };
        let mut output = serde_json::to_vec(&reply).expect("Reply serialization is infallible");
        output.push(b'\n');
        stdout.write_all(&output).await?;
        stdout.flush().await?;
    }
    Ok(())
}

pub struct Mcp {
    host: Arc<Host>,
    initialized: bool,
    negotiated: bool,
}
impl Mcp {
    pub fn new(host: Arc<Host>) -> Self {
        Self {
            host,
            initialized: false,
            negotiated: false,
        }
    }
    pub async fn request(&mut self, mut input: Value) -> Option<Value> {
        let Some(envelope)=input.as_object() else {
            return Some(rpc_error(Value::Null,-32600,"Invalid JSON-RPC request."));
        };
        if envelope.get("jsonrpc").and_then(Value::as_str)!=Some("2.0")
            || !envelope.get("method").is_some_and(Value::is_string)
            || envelope.get("params").is_some_and(|value|!value.is_object())
        {
            return Some(rpc_error(Value::Null,-32600,"Invalid JSON-RPC request."));
        }
        let method=envelope["method"].as_str().unwrap().to_string();
        let Some(id)=envelope.get("id").cloned() else {
            if method=="notifications/initialized" && self.negotiated {
                self.initialized = true;
            }
            return None;
        };
        let valid_id=id.is_string()||id.as_number().is_some_and(|number|number.is_i64()||number.is_u64());
        if !valid_id || method.starts_with("notifications/") {
            return Some(rpc_error(Value::Null,-32600,"Invalid JSON-RPC request."));
        }
        let result = match method.as_str() {
            "initialize" => {
                let version = input
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str);
                if version.is_none()
                    || !input
                        .pointer("/params/capabilities")
                        .is_some_and(Value::is_object)
                    || input
                        .pointer("/params/clientInfo/name")
                        .and_then(Value::as_str)
                        .is_none()
                    || input
                        .pointer("/params/clientInfo/version")
                        .and_then(Value::as_str)
                        .is_none()
                {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Initialize requires protocolVersion, capabilities and clientInfo name/version.",
                    ));
                }
                // The protocol requires a supported version in the response even
                // when the client proposes a different one. It can then disconnect.
                self.negotiated = true;
                Ok(
                    json!({"protocolVersion":MCP_VERSION,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"Herbert","version":env!("CARGO_PKG_VERSION")},"instructions":"Read each tool description before use. inputSchema defines accepted input. outputSchema defines structuredContent. Each call returns schemaVersion, ok, result, and error. isError is true when ok is false. Accepted run IDs remain valid after the request ends. Use run_cancel to request cancellation. Host exit interrupts active execution. Durable journals remain for recovery. Call help_read and read the agent-instructions document before collection."}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" if self.initialized => Ok(
                json!({"tools":OPERATIONS.iter().map(|name|json!({"name":name.replace('.',"_"),"description":description(name),"inputSchema":operation_input_schema(name),"outputSchema":operation_output_schema(name),"annotations":{"readOnlyHint":matches!(*name,"schema"|"help.read"|"about"|"capabilities"|"profiles.list"|"credentials.status"|"collection.preferences.get"|"run.status"|"decompiler.status"|"run.list"|"companies.list"|"serverChanges.preview"|"serverChanges.list"|"serverChanges.inspect"|"serverCollection.inspect"),"openWorldHint":true}})).collect::<Vec<_>>()}),
            ),
            "tools/call" if self.initialized => {
                let name = input
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let Some(operation) = OPERATIONS.iter().find(|op| op.replace('.', "_") == name)
                else {
                    return Some(rpc_error(id, -32602, "Herbert has no tool with that name. Call tools/list to see the available tools."));
                };
                let args = input
                    .pointer_mut("/params/arguments")
                    .map(Value::take)
                    .unwrap_or(json!({}));
                let reply = self
                    .host
                    .execute(Command {
                        schema_version: 1,
                        method: (*operation).into(),
                        arguments: args,
                    })
                    .await;
                let result = json!(reply);
                let mut content = vec![json!({"type":"text","text":serde_json::to_string(&result).unwrap()})];
                // D-M2 (additive): a plain-language error and its next step for clients that
                // show text. content[0] and structuredContent are unchanged.
                if let Some(error) = reply.error.as_ref().filter(|_| !reply.ok) {
                    content.push(json!({"type":"text","text":crate::next_steps::mcp_text(error)}));
                }
                Ok(json!({"content":content,"structuredContent":result,"isError":!reply.ok}))
            }
            "tools/list" | "tools/call" => Err((-32002, "Initialize the MCP connection first.")),
            _ => Err((-32601, "Method not found.")),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err((code, message)) => rpc_error(id, code, message),
        })
    }
}
pub async fn mcp(host: Arc<Host>) -> std::io::Result<()> {
    let mut server = Mcp::new(host);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = zeroize::Zeroizing::new(line);
        let result = match serde_json::from_str::<Value>(&line) {
            Ok(input) => server.request(input).await,
            Err(_) => Some(rpc_error(Value::Null, -32700, "The request is not valid JSON. Send one complete JSON-RPC message per line.")),
        };
        if let Some(output) = result {
            let mut bytes = serde_json::to_vec(&output).unwrap();
            bytes.push(b'\n');
            stdout.write_all(&bytes).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub(crate) fn description(name: &str) -> &'static str {
    match name {
        "serverChanges.preview"=>"Preview possible server changes without mutating Epicor. Supply fullBackup and run using the backup.start or retrieve.start arguments. Include a stable requestId and explicit helper/access choices. The result contains previewId, materialDigest, materialScope, display, unresolved, authorizationRequired, readOnly and matchingAuthorizations. Pass previewId and materialDigest to serverChanges_authorize. Preview does not invoke helpers, borrow access, refresh sessions, export files or reconcile resources. Present material effects before explicit authorization unless a matching grant already applies.",
        "serverChanges.authorize"=>"Explicitly authorize a host-issued preview. Supply previewId, materialDigest and lifetime. Choose oneRun, session, or workflow with workflowId. Optional expiresAt is a Unix timestamp chosen by the user. The result contains authorization and reconciled. A client acknowledgement does not prove human reading. Clients acting for a person must obtain explicit authorization first. Ordinary download requests and preferences cannot create grants. Repeat the same acknowledgement to reconcile its grant. Grant creation activates no schedule.",
        "serverChanges.list"=>"List saved authorizations for this local user. Supply no arguments. The result contains authorizations with material scope, lifetime, origin, expiry and revocation. This operation makes no Epicor request. Workflow authority does not activate unattended execution.",
        "serverChanges.inspect"=>"Read one saved server-change authorization. Set id to its authorization ID. The result contains scope, digest, lifetime, origin, expiry and revocation. Inspection makes no Epicor request.",
        "serverChanges.revoke"=>"Revoke one saved authorization. Set id to its authorization ID. The result contains the retained grant with revokedAt. New optional mutations stop at their next authority check. Proven owned cleanup retains its original attribution and ownership checks. Revocation does not delete native resources.",
        "decompiler.start" => "Decompile a local managed assembly. Supply an absolute assembly path and a new absolute output directory. For external portable symbols, supply symbols and symbolMode=externalPortable. The result contains accepted and jobId. Poll decompiler_status. Herbert retains input snapshots and writes a Project tree. This operation requires no Epicor connection.",
        "decompiler.status" => "Read a raw decompiler job. Set id to its jobId. The result contains saved state, active, input metadata, output metadata, result files, hashes, and diagnostics. Active jobs also report statistics. A partial result can contain useful files.",
        "decompiler.cancel" => "Request cancellation of a raw decompiler job owned by this host. Set id to its jobId. The result contains cancelRequested. Poll decompiler_status for the final state. Herbert preserves input snapshots for recovery.",
        "decompiler.recover" => "Resume an interrupted raw decompiler job from verified snapshots. Set id to its jobId. The result contains jobId and accepted, alreadyFinished, or recoveredCommit. If accepted is true, poll decompiler_status. Recovery requires exclusive output ownership.",
        "collection.preferences.get" => "Read the saved default collection method. Supply no arguments. The result contains collectionMethod. This operation does not change active runs.",
        "collection.preferences.set" => "Save the GUI collection-method preference. Set collectionMethod to directOnly or serverAssisted. The result contains the saved preference. API omission remains Direct only. A saved preference is not server-change authorization. Existing runs retain their recorded method.",
        "schema" => "Read the shared command catalog. Supply no arguments. The result contains schemaVersion, framing, command, and operations with descriptions, inputSchema, and outputSchema. MCP names replace operation dots with underscores.",
        "help.read" => "Read the embedded guides. Supply no arguments. The result contains documents with names and contents. This operation requires no connection and writes no files.",
        "about" => "Read product identity and credits. Supply no arguments. The result contains version, creator, dedication, and public profile links. This operation requires no connection.",
        "helpers.reconcile" => "Reconcile temporary native resources for an inactive helper operation as its original caller. Set id to the exact helper operation UUID. The result contains reconciliation and cleanup evidence. Ownership and source checks can reject removal. This operation can change the authorized server.",
        "plugin.configure" => "Save options for an installed plugin. Supply category, optional selected, and values declared by capabilities. The result contains the plugin definition reply. Herbert saves plugin state beside the module. This operation cannot contact Epicor.",
        "profiles.list" => "List saved connection profiles. Supply no arguments. The result contains an array of profiles with endpoint, defaults, and optional file references. It contains no credential values.",
        "profiles.upsert" => "Create or replace a local connection profile. Supply id and url. Optionally supply name, authMode, defaults, userFile and allowUntrustedCertificate. The result contains saved=true and warnings such as COMPANY_NOT_SET. Herbert stores credentials separately.",
        "profiles.rename" => "Change a saved profile display name. Supply id and name. The result contains renamed=true. The endpoint and credential binding remain unchanged.",
        "profiles.remove" => "Remove a local profile. Supply id. The result contains removed=true and credentialCleanup guidance. To remove its secure credential, call credentials_forget before removing the profile.",
        "profiles.associateFile" => "Associate a validated environment file with a profile. Supply id, path, and the exact environmentId. The result contains associated=true and inputFileChanged=false. Herbert reads the input file without editing it.",
        "profiles.removeFile" => "Remove a profile's saved file reference. Supply id. The result contains removedReference=true and inputFileChanged=false. Herbert preserves the referenced file.",
        "credentials.status" => "Check secure credential presence for a saved profile and endpoint. Supply id. The result contains status with saved or notSaved. It never contains credential values.",
        "credentials.save" => "Save endpoint-bound credentials in the operating system secure store. Supply id and credentials with username and password. Basic API key mode also requires apiKey. The result contains saved=true. Supply secrets through MCP standard input.",
        "credentials.forget" => "Remove the secure credential bound to a saved profile endpoint. Supply id. The result contains forgotten=true. The profile remains.",
        "settings.inspect" => "Read effective host operating settings. Supply no arguments. The result contains effective, timing, and measurement. First use can initialize or migrate the saved decompiler worker preference. These settings are operating choices, not workload limits.",
        "settings.update" => "Change host operating settings. Supply the effective settings object from settings_inspect. The result contains updated=true and effectiveTiming. New work uses changes at the stated boundaries. Herbert persists localDecompilerWorkers.",
        "serverCollection.inspect" => "Inspect the local server collection intent. Supply no arguments. The result contains the recorded invocationId, runId, plugin, and state, or null. This operation makes no server request.",
        "serverCollection.reconcile" => "Reconcile a stranded collection intent after independent confirmation that server work stopped. Supply invocationId, confirmedServerStopped=true, and nonempty evidence. The result contains state=operatorConfirmedStopped. Herbert rejects an active owner or a mismatched ID. This operation does not stop server work.",
        "capabilities" => "Discover plugins in the selected folder. Supply no arguments. The result contains categories, diagnostics, targets, and temporaryAccess guidance. Categories describe availability, selection, and settings. Discovery does not contact Epicor.",
        "connection.test" => "Test authentication and admission, then attempt logout. Supply credentialSource and the connection scope. Omit company to list the companies this account can use. Optional allowUntrustedCertificate accepts a self-signed or private-CA certificate for this endpoint only. The result contains connection details, availableCompanies, companySource, selectedCompany, transportTrust and cleanup=null when successful. Failed logout retains the session. If error.context.connectionRetained is true, call connection_disconnect in this same host before another login. Valid connection settings can change request scheduling before authentication.",
        "connection.connect" => "Open this host's single Epicor connection. Supply credentialSource and the required connection scope, including company. Optional allowUntrustedCertificate applies to this endpoint only. The result contains native connection details and transportTrust. Failed admission with failed logout retains a cleanup session. If error.context.connectionRetained is true, call connection_disconnect in this same host. Valid connection settings can change request scheduling before authentication.",
        "connection.disconnect" => "Attempt logout for this host's connected or retained cleanup session. Supply no arguments. The result contains disconnected=true after confirmed logout. Active network runs prevent disconnect. If logout fails, the host retains the session for another attempt.",
        "companies.list" => "Discover companies through the active authorized connection. Supply no arguments. The result contains the native companies response and scope guidance. Each collection run targets one company.",
        "retrieve.start" => "Start collection of exact native identities through the active connection. Supply requestId, destination, outputMode, and identities. Exact identities select roots independently of saved discovery filters. Processing options still apply. Identities select categories. The categories field does not select targeted scope. pluginSettings overrides do not save options. The result contains accepted, runId, and destination. Poll run_status and inspect completeness within the exact selection.",
        "backup.start" => "Start discovery within selected categories and effective plugin options through the active connection. Supply requestId, destination, and outputMode. Omitted categories select all available categories. Unavailable categories cause rejection. Omitted pluginSettings inherit saved plugin values. Explicit blank filter overrides clear selection filters for this run without saving. Valid zero-match discovery is an empty selection, not a read failure. Settings and module fingerprints bind recovery. The scope value fullBackup is the persisted discovery mode, not a claim of full-environment coverage. The result contains accepted, runId, and destination. Poll run_status and inspect completeness within the frozen selection.",
        "run.status" => "Read a saved or active run. Set id to its runId. Optionally set summary=true for a compact view. The result contains phase, executionActive, progress, and diagnostics. The full view includes artifact evidence. Inspect completeness before accepting results.",
        "diagnostics.export" => "Write a sanitized support ZIP without network access. Supply a new path. Optionally supply a run id. Without id, Herbert uses the last diagnostic before a run. The result contains saved, path, sanitized, schemaVersion, and event counts. Herbert preserves existing target files.",
        "run.list" => "List locally saved runs. Supply no arguments. The result contains run summaries or diagnostics for unreadable records. Use run_status to inspect completeness.",
        "run.pause" => "Pause new dispatch for an active run. Set id to its runId. The result contains accepted, runId, and effect. Current work can finish. Terminal runs return accepted=false.",
        "run.resume" => "Resume dispatch for a paused active run. Set id to its runId. The result contains accepted, runId, and effect. Terminal runs return accepted=false.",
        "run.cancel" => "Request cancellation of an active run. Set id to its runId. The result contains accepted, runId, and effect. Poll run_status for completion and cleanup. Herbert preserves verified files. Terminal runs return accepted=false.",
        "run.recover" => "Resume an interrupted network run. Set id to its runId. Use the original endpoint, company, and caller. Reconcile outstanding access first. The result contains accepted, runId, operation, and destination. Poll run_status.",
        "run.retryFailed" => "Retry failed or incomplete acquisition in a saved run. Set id to its runId. Use the original connection scope. This operation can acquire server data again. The result contains accepted, runId, operation, and destination. Poll run_status.",
        "run.reprocess" => "Reprocess saved definitions and files locally. Set id to its runId. The result contains accepted, runId, operation, and destination. Poll run_status. This operation requires no Epicor connection and does not acquire native data.",
        _ => unreachable!("Unknown shared operation: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::operation_schema;
    #[test]
    fn every_mcp_tool_explains_arguments_results_and_shared_schema() {
        let shared = operation_schema();
        let operations = shared["operations"].as_array().unwrap();
        assert_eq!(operations.len(), OPERATIONS.len());
        for name in OPERATIONS {
            let documentation = description(name);
            assert!(documentation.contains("Supply") || documentation.contains("Set "), "{name} has no argument guidance");
            assert!(documentation.contains("The result contains"), "{name} has no result guidance");
            assert!(!documentation.contains("Shared Herbert operation"), "{name} still has fallback text");
            assert!(!documentation.contains(';'), "{name} contains a semicolon");
            for sentence in documentation.split(['.','?','!']) {
                let words=sentence.split_whitespace().count();
                assert!(words<=25,"{name} contains a {words}-word sentence");
            }
            let command = operations.iter().find(|entry| entry["name"] == *name).unwrap();
            assert_eq!(command["description"], documentation);
            assert_eq!(command["inputSchema"], operation_input_schema(name));
            assert_eq!(command["outputSchema"], operation_output_schema(name));
        }
    }
    #[tokio::test]
    async fn every_mcp_tool_publishes_its_structured_output_schema() {
        let dir=tempfile::tempdir().unwrap();
        let host=Host::new(dir.path().into(),dir.path().join("plugins"));
        let mut server=Mcp::new(host);
        server.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
        server.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        let reply=server.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();
        let tools=reply["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(),OPERATIONS.len());
        for operation in OPERATIONS {
            let name=operation.replace('.',"_");
            let tool=tools.iter().find(|tool|tool["name"]==name).unwrap();
            // MCP 2025-11-25 Tool.outputSchema requires this root field,
            // even when every oneOf branch already describes an object.
            assert_eq!(tool["outputSchema"]["type"], "object", "{name}");
            assert_eq!(tool["outputSchema"],operation_output_schema(operation));
        }
    }
    #[tokio::test]
    async fn collection_method_is_shared_and_persists_across_frontends() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let initial = host.execute(Command {
            schema_version: 1,
            method: "collection.preferences.get".into(),
            arguments: json!({}),
        }).await;
        assert_eq!(initial.result.unwrap()["collectionMethod"], "directOnly");
        let mut mcp = Mcp::new(host.clone());
        mcp.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}
        }})).await;
        mcp.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        for mode in ["directOnly", "serverAssisted"] {
            let reply = mcp.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"collection_preferences_set","arguments":{"collectionMethod":mode}
            }})).await.unwrap();
            assert_eq!(reply["result"]["isError"], false);
            let fresh = Host::new(dir.path().into(), dir.path().join("plugins"));
            let read = fresh.execute(Command {
                schema_version: 1,
                method: "collection.preferences.get".into(),
                arguments: json!({}),
            }).await;
            assert_eq!(read.result.unwrap()["collectionMethod"], mode);
        }
        let rejected = host.execute(Command {
            schema_version: 1,
            method: "collection.preferences.get".into(),
            arguments: json!({"collectionMethod":"directOnly"}),
        }).await;
        assert!(!rejected.ok);
    }
    #[tokio::test]
    async fn error_replies_add_plain_text_with_a_next_step_and_keep_structured_content() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut mcp = Mcp::new(host);
        mcp.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}
        }})).await;
        mcp.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        let call = |id: u64, arguments: Value| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"collection_preferences_get","arguments":arguments}});
        let ok = mcp.request(call(2, json!({}))).await.unwrap();
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(ok["result"]["content"].as_array().unwrap().len(), 1, "success replies are unchanged");
        let failed = mcp.request(call(3, json!({"collectionMethod":"directOnly"}))).await.unwrap();
        let result = &failed["result"];
        assert_eq!(result["isError"], true);
        let content = result["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        // content[0] is still the serialized structured result.
        assert_eq!(serde_json::from_str::<Value>(content[0]["text"].as_str().unwrap()).unwrap(), result["structuredContent"]);
        let error: herbert_contract::Diagnostic = serde_json::from_value(result["structuredContent"]["error"].clone()).unwrap();
        assert_eq!(content[1], json!({"type":"text","text":crate::next_steps::mcp_text(&error)}));
        assert!(content[1]["text"].as_str().unwrap().starts_with(&error.message));
    }
    #[tokio::test]
    async fn cli_operation_and_mcp_have_identical_semantic_results() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let cli = host
            .execute(Command {
                schema_version: 1,
                method: "schema".into(),
                arguments: json!({}),
            })
            .await;
        let mut server = Mcp::new(host);
        assert!(server.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await.unwrap().get("error").is_none());
        server
            .request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        let mcp=server.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"schema","arguments":{}}})).await.unwrap();
        assert_eq!(mcp.pointer("/result/structuredContent"), Some(&json!(cli)));
        assert_eq!(cli.result.unwrap(), operation_schema());
    }
    #[tokio::test]
    async fn initialization_notification_cannot_skip_negotiation() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = Mcp::new(host);
        server
            .request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        assert_eq!(
            server
                .request(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .await
                .unwrap()["error"]["code"],
            -32002
        );
        let reply=server.request(json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"future-client-version","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await.unwrap();
        assert_eq!(reply["result"]["protocolVersion"], MCP_VERSION);
        assert!(!server.initialized);
        server
            .request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        assert!(server.initialized);
    }

    #[tokio::test]
    async fn invalid_or_notification_envelopes_do_not_dispatch_tools() {
        let dir=tempfile::tempdir().unwrap();
        let host=Host::new(dir.path().into(),dir.path().join("plugins"));
        let mut server=Mcp::new(host.clone());
        server.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
        server.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        let mutation=|id:Option<Value>,mode:&str| {
            let mut value=json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"collection_preferences_set","arguments":{"collectionMethod":mode}}});
            if let Some(id)=id { value["id"]=id; }
            value
        };
        assert!(server.request(mutation(None,"serverAssisted")).await.is_none());
        for id in [Value::Null,json!(true),json!({}),json!(1.5)] {
            let reply=server.request(mutation(Some(id),"serverAssisted")).await.unwrap();
            assert_eq!(reply["error"]["code"],-32600);
        }
        let saved=host.execute(Command {schema_version:1,method:"collection.preferences.get".into(),arguments:json!({})}).await.result.unwrap();
        assert_eq!(saved["collectionMethod"],"directOnly");
        for id in [json!(2),json!("two")] {
            let reply=server.request(mutation(Some(id.clone()),"serverAssisted")).await.unwrap();
            assert_eq!(reply["id"],id);
            assert_eq!(reply["result"]["isError"],false);
            let reset=host.execute(Command {schema_version:1,method:"collection.preferences.set".into(),arguments:json!({"collectionMethod":"directOnly"})}).await;
            assert!(reset.ok,"{:?}",reset.error);
        }
        assert_eq!(server.request(json!({"jsonrpc":"2.0","id":3,"method":"notifications/initialized"})).await.unwrap()["error"]["code"],-32600);
        assert!(host.shutdown().await.is_empty());
    }

    // ---- MCP parity for the certificate option and Test connection.
    use crate::application::operation_result_schema;
    use crate::test_http::Server;
    async fn mcp(host: Arc<Host>) -> Mcp {
        let mut server = Mcp::new(host);
        server.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await;
        server.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
        server
    }
    async fn call(server: &mut Mcp, name: &str, arguments: Value) -> Value {
        let reply = server.request(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await.unwrap();
        reply["result"]["structuredContent"].clone()
    }
    fn epicor() -> crate::test_http::Router {
        Arc::new(|method: &str, _: &str, _: &Value| match method {
            "GetUserFile" => (200, json!({"returnObj":{"UserFile":[{"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}],
                "UserComp":[{"Company":"EPIC","CompanyName":"Epic Corp"}]}})),
            "GetCurrentValuesFromServer" => (200, json!({"parameters":{"companyID":"EPIC"}})),
            "GetDatabaseVersion" => (200, json!({"returnObj":"5.2.100"})),
            _ => (200, json!({})),
        })
    }
    fn arguments(server: &Server, company: Option<&str>, allow: Option<bool>) -> Value {
        let mut arguments = json!({"url":server.endpoint,"authMode":"basicApiKey",
            "credentialSource":{"type":"inline","credentials":{"username":"manager","password":"fixture","apiKey":"fixture"}}});
        if let Some(company) = company { arguments["company"] = json!(company); }
        if let Some(allow) = allow { arguments["allowUntrustedCertificate"] = json!(allow); }
        arguments
    }
    /// Reply-schema conformance for the subset the declared schemas use: type
    /// (single or list), enum, const, required, closed and schema-valued
    /// additionalProperties, items, oneOf (exactly one), anyOf, allOf, and
    /// `$ref` into the nearest enclosing `$defs` (each embedded schemars root
    /// carries its own), plus numeric, length and count bounds. Any other keyword
    /// fails, so a schema can never pass because its constraint went unchecked.
    /// No validator crate is in the lockfile; none is added.
    fn conforms(value: &Value, schema: &Value) -> Result<(), String> {
        check(value, schema, &Value::Null)
    }
    const CHECKED: &[&str] = &["type", "enum", "const", "required", "properties", "additionalProperties", "items", "oneOf", "anyOf", "allOf", "$ref",
        "minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "minLength", "maxLength", "minItems", "maxItems", "uniqueItems", "minProperties", "maxProperties"];
    const ANNOTATIONS: &[&str] = &["description", "title", "default", "examples", "$schema", "$defs", "format", "writeOnly", "readOnly", "deprecated"];
    fn check(value: &Value, schema: &Value, defs: &Value) -> Result<(), String> {
        let defs = if schema.get("$defs").is_some() { &schema["$defs"] } else { defs };
        if schema == &Value::Bool(true) || schema.as_object().is_some_and(|o| o.is_empty()) { return Ok(()); }
        if schema == &Value::Bool(false) { return Err(format!("{value} matches a false schema")); }
        if let Some(unknown) = schema.as_object().and_then(|o| o.keys().find(|k| !CHECKED.contains(&k.as_str()) && !ANNOTATIONS.contains(&k.as_str()))) {
            return Err(format!("unsupported schema keyword {unknown}"));
        }
        let bound = |key: &str| schema.get(key).and_then(Value::as_f64);
        if let Some(n) = value.as_f64() {
            if bound("minimum").is_some_and(|b| n < b) || bound("maximum").is_some_and(|b| n > b)
                || bound("exclusiveMinimum").is_some_and(|b| n <= b) || bound("exclusiveMaximum").is_some_and(|b| n >= b) {
                return Err(format!("{value} is outside its bounds"));
            }
        }
        let count = |key: &str| schema.get(key).and_then(Value::as_u64).map(|b| b as usize);
        let within = |len: usize, min: &str, max: &str| count(min).is_none_or(|b| len >= b) && count(max).is_none_or(|b| len <= b);
        if let Some(text) = value.as_str() { if !within(text.chars().count(), "minLength", "maxLength") { return Err(format!("{value} has the wrong length")); } }
        if let Some(list) = value.as_array() {
            if !within(list.len(), "minItems", "maxItems") { return Err(format!("{value} has the wrong number of items")); }
            if schema["uniqueItems"] == true && list.iter().enumerate().any(|(i, a)| list[..i].contains(a)) { return Err(format!("{value} repeats an item")); }
        }
        if let Some(object) = value.as_object() { if !within(object.len(), "minProperties", "maxProperties") { return Err(format!("{value} has the wrong number of fields")); } }
        if let Some(reference) = schema["$ref"].as_str() {
            let name = reference.strip_prefix("#/$defs/").ok_or_else(|| format!("unsupported $ref {reference}"))?;
            let target = defs.get(name).ok_or_else(|| format!("unresolved $ref {reference}"))?;
            check(value, target, defs)?;
        }
        if let Some(branches) = schema["oneOf"].as_array() {
            let matched = branches.iter().filter(|branch| check(value, branch, defs).is_ok()).count();
            if matched != 1 { return Err(format!("{matched} oneOf branches match {value}")); }
        }
        if let Some(branches) = schema["anyOf"].as_array() {
            if !branches.iter().any(|branch| check(value, branch, defs).is_ok()) { return Err(format!("no anyOf branch matches {value}")); }
        }
        for branch in schema["allOf"].as_array().into_iter().flatten() { check(value, branch, defs)?; }
        if let Some(types) = match &schema["type"] { Value::String(t) => Some(vec![t.as_str()]), Value::Array(t) => Some(t.iter().filter_map(Value::as_str).collect()), _ => None } {
            let is = |t: &str| match t {
                "null" => value.is_null(), "boolean" => value.is_boolean(), "string" => value.is_string(),
                "object" => value.is_object(), "array" => value.is_array(), "number" => value.is_number(),
                "integer" => value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0),
                _ => false,
            };
            if !types.iter().any(|t| is(t)) { return Err(format!("{value} is not of type {types:?}")); }
        }
        if let Some(allowed) = schema["enum"].as_array() { if !allowed.contains(value) { return Err(format!("{value} is not one of {allowed:?}")); } }
        if let Some(constant) = schema.get("const") { if constant != value { return Err(format!("{value} != const {constant}")); } }
        if let Some(object) = value.as_object() {
            for key in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if !object.contains_key(key) { return Err(format!("missing required {key}")); }
            }
            let properties = schema["properties"].as_object();
            for (key, item) in object {
                match properties.and_then(|p| p.get(key)) {
                    Some(property) => check(item, property, defs).map_err(|e| format!("{key}: {e}"))?,
                    None if schema["additionalProperties"] == false => return Err(format!("undeclared field {key}")),
                    None if schema["additionalProperties"].is_object() => check(item, &schema["additionalProperties"], defs).map_err(|e| format!("{key}: {e}"))?,
                    None => {}
                }
            }
        }
        if let (Some(items), Some(values)) = (schema.get("items"), value.as_array()) {
            for item in values { check(item, items, defs)?; }
        }
        Ok(())
    }

    #[test]
    fn reply_conformance_enforces_type_enum_null_branches_and_refs() {
        let nullable = json!({"oneOf":[{"type":"string"},{"type":"null"}]});
        assert!(conforms(&json!(null), &nullable).is_ok());
        assert!(conforms(&json!(7), &nullable).is_err(), "a null branch no longer matches anything");
        assert!(conforms(&json!("x"), &json!({"type":["string","null"]})).is_ok());
        assert!(conforms(&json!(true), &json!({"type":["string","null"]})).is_err());
        assert!(conforms(&json!("verified"), &json!({"enum":["verified","unencrypted"]})).is_ok());
        assert!(conforms(&json!("other"), &json!({"enum":["verified","unencrypted"]})).is_err());
        let trust = crate::application::operation_result_schema("connection.connect")["properties"]["transportTrust"].clone();
        assert!(conforms(&json!({"mode":"relaxedUntrustedIssuer"}), &trust).is_ok());
        assert!(conforms(&json!({"mode":"trustedBySomeoneElse"}), &trust).is_err(), "refs resolve and the enum is enforced");
        assert!(conforms(&json!({"mode":"verified","unexpected":1}), &trust).is_err(), "closed objects stay closed through refs");
    }

    #[test]
    fn reply_conformance_is_closed_strict_and_refuses_unknown_keywords() {
        let closed = json!({"type":"object","additionalProperties":false});
        assert!(conforms(&json!({}), &closed).is_ok());
        assert!(conforms(&json!({"extra":1}), &closed).is_err(), "closed without properties still refuses fields");
        let overlapping = json!({"oneOf":[{"type":"integer"},{"type":"number"}]});
        assert!(conforms(&json!(3), &overlapping).is_err(), "two matching oneOf branches must fail");
        assert!(conforms(&json!(3.5), &overlapping).is_ok());
        assert!(conforms(&json!(1.5), &json!({"type":"integer"})).is_err());
        assert!(conforms(&json!(2.0), &json!({"type":"integer"})).is_ok(), "JSON Schema numeric equality: 2.0 is an integer");
        assert_eq!(conforms(&json!("x"), &json!({"type":"string","pattern":"^y$"})).unwrap_err(), "unsupported schema keyword pattern");
        assert!(conforms(&json!(1), &json!({"type":"integer","description":"d","format":"uint32","default":0,"deprecated":false})).is_ok());
        assert!(conforms(&json!(-1), &json!({"type":"integer","minimum":0})).is_err());
        assert!(conforms(&json!("ab"), &json!({"maxLength":1})).is_err());
        assert!(conforms(&json!([1, 1]), &json!({"uniqueItems":true})).is_err());
        assert!(conforms(&json!(null), &json!(false)).is_err());
    }

    #[tokio::test]
    async fn mcp_connection_test_reports_certificate_codes_and_transport_trust() {
        let tls = Server::start(Some("self-signed"), epicor());
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = mcp(host.clone()).await;
        let refused = call(&mut server, "connection_test", arguments(&tls, Some("EPIC"), Some(false))).await;
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["error"]["code"], "CERTIFICATE_UNTRUSTED");
        assert_eq!(refused["error"]["retryable"], false);
        let accepted = call(&mut server, "connection_test", arguments(&tls, Some("EPIC"), Some(true))).await;
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(accepted["result"]["transportTrust"]["mode"], "relaxedUntrustedIssuer");
        assert!(accepted["result"]["transportTrust"]["certificateSha256"].as_str().is_some_and(|sha| sha.len() == 64));
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn mcp_profiles_upsert_and_list_round_trip_allow_untrusted_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = mcp(host.clone()).await;
        let saved = call(&mut server, "profiles_upsert", json!({"id":"test","url":"https://test.example.invalid/Kinetic","allowUntrustedCertificate":true,"defaults":{"company":"EPIC"}})).await;
        assert_eq!(saved["ok"], true, "{saved}");
        call(&mut server, "profiles_upsert", json!({"id":"plain","url":"https://plain.example.invalid/Kinetic","defaults":{"company":"EPIC"}})).await;
        let listed = call(&mut server, "profiles_list", json!({})).await;
        let profiles = listed["result"].as_array().unwrap();
        let find = |id: &str| profiles.iter().find(|p| p["id"] == id).unwrap().clone();
        assert_eq!(find("test")["allowUntrustedCertificate"], true);
        assert!(find("plain").get("allowUntrustedCertificate").is_none(), "off is the default and is not written");
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn mcp_capabilities_and_help_describe_allow_untrusted_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = mcp(host.clone()).await;
        let capabilities = call(&mut server, "capabilities", json!({})).await;
        assert_eq!(capabilities["result"]["connection"], json!({"allowUntrustedCertificate":true,"companyDiscovery":true}));
        assert!(capabilities["result"]["temporaryAccess"].get("qualified").is_none(), "the hard-coded qualified flag is retired");
        let schema = operation_result_schema("capabilities");
        assert!(schema["properties"]["temporaryAccess"]["properties"].get("qualified").is_none());
        assert_eq!(schema["properties"]["qualification"]["description"], "How to judge completeness: check each artifact's result and diagnostics.");
        assert_eq!(schema["properties"]["categories"]["items"]["properties"]["qualification"]["description"], "The plugin's plain statement of how to judge its results, or why it is unavailable.");
        let help = call(&mut server, "help_read", json!({})).await.to_string();
        assert!(help.contains("allowUntrustedCertificate") && help.contains("CERTIFICATE_UNTRUSTED"), "embedded MCP guide describes the option");
        let tools = server.request(json!({"jsonrpc":"2.0","id":9,"method":"tools/list"})).await.unwrap();
        for read_only in ["serverChanges_preview","serverChanges_list","serverChanges_inspect","serverCollection_inspect"] {
            let tool = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == read_only).unwrap();
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{read_only}");
        }
        let init = Mcp::new(host.clone()).request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":MCP_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await.unwrap();
        assert!(init["result"]["instructions"].as_str().unwrap().contains("read the agent-instructions document"));
        assert!(host.shutdown().await.is_empty());
    }

    fn lease_outcome_journal() -> crate::storage::RunJournal {
        let mut journal = crate::storage::new_journal("lease-outcomes".into());
        journal.diagnostics.push(herbert_contract::Diagnostic::new("SERVER_LEASE_LOST", "fixture"));
        journal.diagnostics.push(herbert_contract::Diagnostic::new("SERVER_LEASE_LOST_DURING_CALL", "fixture"));
        journal.diagnostics.push(herbert_contract::Diagnostic::new("CLEANUP_AUTHORITY_REQUIRED", "fixture"));
        journal.foreign_pending_helpers.push(crate::storage::ForeignPendingHelper { library_id: "HX".into(), global_id: uuid::Uuid::new_v4().to_string(), role: "host".into(), state: "unknown".into() });
        journal
    }

    #[test]
    fn mcp_run_status_and_summary_declare_server_change_outcomes() {
        let schema = operation_result_schema("run.status");
        let declared = schema["properties"]["serverChanges"]["properties"]["resources"]["items"]["properties"]["outcome"].to_string();
        for outcome in ["noChangesObserved", "removedVerified", "restoredVerified", "leaseReleasedSharedRemains", "invokedPreexisting", "pending", "uncertain", "failed", "unknown", "foreignPending"] {
            assert!(declared.contains(outcome), "{outcome} is part of the one published outcome enum");
        }
        assert!(schema["properties"]["foreignPendingHelpers"]["description"].as_str().unwrap().contains("use serverChanges"));
        let journal = lease_outcome_journal();
        let full = crate::progress_snapshot::full(&journal);
        conforms(&full["serverChanges"], &schema["properties"]["serverChanges"]).unwrap();
        let summary = crate::progress_snapshot::summary(&journal);
        assert_eq!(summary["serverChanges"], full["serverChanges"], "summary and full status report the same outcomes");
    }

    #[test]
    fn mcp_run_status_reports_foreign_pending_and_lease_outcomes() {
        let status = crate::progress_snapshot::full(&lease_outcome_journal());
        let changes = &status["serverChanges"];
        assert_eq!(changes["leaseLost"], true);
        assert_eq!(changes["resources"].as_array().unwrap().iter().map(|r| (r["code"].as_str().unwrap(), r["outcome"].as_str().unwrap())).collect::<Vec<_>>(),
            vec![("SERVER_LEASE_LOST_DURING_CALL", "uncertain"), ("CLEANUP_AUTHORITY_REQUIRED", "pending")]);
        assert_eq!(changes["foreignPending"][0]["state"], "unknown");
        assert!(changes["resources"].as_array().unwrap().iter().all(|r| r["outcome"] != "foreignPending"), "foreign-pending items never appear in resources");
        // The new codes reach MCP unchanged.
        let text = status["diagnostics"].to_string();
        for code in ["SERVER_LEASE_LOST", "SERVER_LEASE_LOST_DURING_CALL", "CLEANUP_AUTHORITY_REQUIRED"] { assert!(text.contains(code), "{code}"); }
    }

    #[tokio::test]
    async fn mcp_connect_and_test_replies_validate_against_declared_output_schema() {
        let plain = Server::start(None, epicor());
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = mcp(host.clone()).await;
        for (operation, company) in [("connection_test", Some("EPIC")), ("connection_test", None), ("connection_connect", Some("EPIC"))] {
            let reply = call(&mut server, operation, arguments(&plain, company, None)).await;
            assert_eq!(reply["ok"], true, "{operation}: {reply}");
            conforms(&reply["result"], &operation_result_schema(&operation.replace('_', "."))).unwrap_or_else(|e| panic!("{operation}: {e}"));
        }
        // Relaxed trust over TLS (self-signed loopback with the option on). connection.test
        // needs no active connection, so release the one connect above made.
        let disconnected = call(&mut server, "connection_disconnect", json!({})).await;
        assert_eq!(disconnected["ok"], true, "{disconnected}");
        let tls = Server::start(Some("self-signed"), epicor());
        for (operation, company) in [("connection_test", Some("EPIC")), ("connection_test", None)] {
            let reply = call(&mut server, operation, arguments(&tls, company, Some(true))).await;
            assert_eq!(reply["ok"], true, "{operation}: {reply}");
            assert_eq!(reply["result"]["transportTrust"]["mode"], "relaxedUntrustedIssuer");
            conforms(&reply["result"], &operation_result_schema(&operation.replace('_', "."))).unwrap_or_else(|e| panic!("relaxed {operation}: {e}"));
        }
        let capabilities = call(&mut server, "capabilities", json!({})).await;
        conforms(&capabilities["result"], &operation_result_schema("capabilities")).unwrap();
        let upsert = call(&mut server, "profiles_upsert", json!({"id":"x","url":"https://x.example.invalid/K"})).await;
        conforms(&upsert["result"], &operation_result_schema("profiles.upsert")).unwrap();
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn mcp_connection_test_returns_company_list_without_company() {
        let plain = Server::start(None, epicor());
        let dir = tempfile::tempdir().unwrap();
        let host = Host::new(dir.path().into(), dir.path().join("plugins"));
        let mut server = mcp(host.clone()).await;
        let reply = call(&mut server, "connection_test", arguments(&plain, None, None)).await;
        assert_eq!(reply["result"]["availableCompanies"], json!([{"id":"EPIC","name":"Epic Corp"}]));
        assert_eq!(reply["result"]["companySource"], "userAccess");
        assert_eq!(reply["result"]["company"], Value::Null);
        // The schema advertises environmentId; environment_id is an alias only, and
        // both spellings produce the identical successful result.
        let input = crate::application::operation_input_schema("connection.test").to_string();
        assert!(input.contains("\"environmentId\"") && !input.contains("\"environment_id\""), "{input}");
        let file = dir.path().join("environments.json");
        std::fs::write(&file, serde_json::to_vec(&json!({"schemaVersion":1,"environments":[{"id":"test","name":"Test","url":plain.endpoint,"authMode":"basicApiKey",
            "credentials":{"username":"manager","password":"fixture","apiKey":"fixture"},"defaults":{"company":"EPIC"}}]})).unwrap()).unwrap();
        let mut replies = Vec::new();
        for key in ["environmentId", "environment_id"] {
            let reply = call(&mut server, "connection_test", json!({"url":plain.endpoint,"authMode":"basicApiKey","company":"EPIC",
                "credentialSource":{"type":"userFile","path":file,(key):"test"}})).await;
            assert_eq!(reply["ok"], true, "{key}: {reply}");
            replies.push(reply["result"].clone());
        }
        assert_eq!(replies[0], replies[1], "the alias gives the identical result");
        assert!(host.shutdown().await.is_empty());
    }
}
