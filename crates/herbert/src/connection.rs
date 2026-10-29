//! Native authentication, admission and transport lifecycle.
//! Hold the shared request permit until the response is consumed. Keep credentials here.
use crate::{
    input::{AuthMode, Credentials},
    scheduler::Budget,
};
use herbert_contract::{Diagnostic, OperatingSettings};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

struct Session {
    token: Zeroizing<String>,
    expires: Instant,
    id: Option<Zeroizing<String>>,
}
#[derive(Clone)]
pub struct NativeResponse {
    pub raw: Vec<u8>,
    pub value: Value,
    pub elapsed_ms: u128,
}
pub struct Connection {
    pub endpoint: Url,
    pub company: String,
    pub mode: AuthMode,
    credentials: Credentials,
    client: Client,
    budget: Arc<Budget>,
    // Lease renewal must not wait behind long-running collection requests.
    // One control slot serializes lease traffic for this connection.
    lease_budget: Arc<Budget>,
    settings: OperatingSettings,
    lifecycle: RwLock<()>,
    session_renewal: Mutex<()>,
    session: Mutex<Option<Session>>,
    caller: Mutex<Option<String>>,
    server_version: Mutex<Option<String>>,
    /// The profile's certificate option and what this connection's TLS observed.
    pub allow_untrusted_certificate: bool,
    transport_trust: crate::transport_trust::Observed,
    /// A company-less connection used only by connection.test to list companies.
    /// It never sets a session company and refuses collection reads and writes.
    discovery: bool,
    /// Set when connection.test retains this connection because its sign-out failed.
    /// A test is never a confirmed connect, so a retained test only waits for disconnect.
    test_only: std::sync::atomic::AtomicBool,
    /// Discovery addresses data routes through the account's default session company.
    discovery_company: std::sync::Mutex<Option<String>>,
    /// UserComp rows and CompList from the last admission read.
    admitted_companies: std::sync::Mutex<Option<Value>>,
    // Test-only HTTP accounting. Never records bodies, headers or queries.
    #[cfg(test)]
    pub(crate) qualification_http: std::sync::Mutex<Vec<Value>>,
}
fn canceled() -> Diagnostic {
    Diagnostic::new("CANCELED", "The request was canceled.")
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ConnectionOptions {
    pub allow_untrusted_certificate: bool,
}
pub(crate) fn company_required() -> Diagnostic {
    Diagnostic::new("COMPANY_REQUIRED", "Choose a company for this connection. Use Test connection, or run connection.test without a company, to list the companies this account can use.")
}
fn transport_error() -> Diagnostic {
    Diagnostic::new(
        "TRANSPORT_FAILED",
        "The Epicor request failed. Check reachability, TLS, timeout settings and response integrity.",
    )
}
pub(crate) fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
}

/// Project only compiler messages. Native account, authentication and general
/// exception bodies are never included in user-facing failure text.
fn collect_compiler_messages(value: &Value, messages: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let is_compiler_message = text
                .as_bytes()
                .windows(6)
                .any(|token| token.starts_with(b"CS") && token[2..].iter().all(u8::is_ascii_digit));
            if is_compiler_message && !messages.contains(text) {
                messages.push(text.clone());
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_compiler_messages(value, messages);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                collect_compiler_messages(value, messages);
            }
        }
        _ => {}
    }
}

impl Connection {
    /// Artifact lifecycle and execution calls never automatically replay. The
    /// coordinator binds them to declared aliases and fresh native ownership.
    pub(crate) async fn artifact_call(
        &self,
        service: &str,
        method: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if self.caller().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Connect and verify the Epicor user before an Epicor artifact operation.",
            ));
        }
        self.ensure_session(cancel).await?;
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        self.send(service, method, arguments, true, false, true, cancel)
            .await
    }
    pub(crate) async fn artifact_function_call(
        &self,
        library: &str,
        function: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.function_call(library, function, arguments, cancel, false).await
    }

    pub(crate) async fn lease_call(
        &self, library: &str, request: Value, cancel: &CancellationToken,
    ) -> Result<Value, Diagnostic> {
        let mut attempt = 0;
        loop {
            let response = self.function_call(library, "Lease", json!({"iRequest":request.to_string()}), cancel, true).await?;
            let text = response.value.get("oJson").or_else(|| response.value.pointer("/returnObj/oJson"))
                .and_then(Value::as_str).ok_or_else(|| Diagnostic::new("LEASE_RESPONSE_INVALID", "The server omitted its lease response."))?;
            let value: Value = serde_json::from_str(text).map_err(|_| Diagnostic::new("LEASE_RESPONSE_INVALID", "The server returned an invalid lease response."))?;
            if value["leaseBusy"] != true { return Ok(value); }
            // The server confirms that admission performed no work: another writer holds the
            // registry. Back off with jitter so contending runs spread out instead of storming it.
            // There is no give-up count; cancellation ends the wait.
            let delay = lease_busy_delay(attempt, uuid::Uuid::new_v4().as_u128() as u16);
            attempt += 1;
            tokio::select! { _ = cancel.cancelled() => return Err(canceled()),
                _ = tokio::time::sleep(delay) => {} }
        }
    }


    async fn function_call(
        &self, library: &str, function: &str, arguments: Value,
        cancel: &CancellationToken, lease: bool,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if !valid_segment(library) || !valid_segment(function) {
            return Err(Diagnostic::new(
                "ARTIFACT_FUNCTION_ID_INVALID",
                "The Epicor library and function must be valid route segments.",
            ));
        }
        if self.caller().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Connect and verify the Epicor user before an Epicor function call.",
            ));
        }
        self.ensure_session(cancel).await?;
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        let mut url = self.endpoint.clone();
        url.path_segments_mut()
            .map_err(|_| Diagnostic::new("INVALID_ENDPOINT", "Cannot form the Function endpoint."))?
            .pop_if_empty()
            .push("api")
            .push("v2")
            .push("efx")
            .push(&self.company)
            .push(library)
            .push(function);
        let request = self
            .client
            .post(url)
            .header("Accept", "application/json")
            .header("callSettings", json!({"Company":self.call_company()}).to_string())
            .json(&arguments);
        let request = self.authenticate(request, true).await?;
        if lease {
            self.consume_once_on(request, false, cancel, &mut None, &self.lease_budget).await
        } else {
            self.consume(request, true, false, cancel).await
        }
    }

    pub fn settings(&self) -> &OperatingSettings {
        &self.settings
    }
    /// A certificate failure gets its own plain, non-retryable diagnostic.
    fn request_error(&self, error: &reqwest::Error) -> Diagnostic {
        crate::transport_trust::certificate_diagnostic(error, self.endpoint.host_str().unwrap_or(""), self.allow_untrusted_certificate)
            .unwrap_or_else(transport_error)
    }
    /// How this connection's server certificate was trusted. Plain http has no
    /// certificate and is reported as unencrypted, never as verified. Over https a
    /// profile without the option uses Windows trust only, so a working
    /// connection is "verified".
    pub fn transport_trust(&self) -> crate::transport_trust::TransportTrust {
        let unobserved = |mode| crate::transport_trust::TransportTrust { mode, certificate_sha256: None, subject: None, issuer: None, not_after: None };
        if self.endpoint.scheme() != "https" {
            return unobserved(crate::transport_trust::TrustMode::Unencrypted);
        }
        self.transport_trust.lock().ok().and_then(|observed| observed.clone()).unwrap_or(unobserved(crate::transport_trust::TrustMode::Verified))
    }
    pub fn is_company_discovery(&self) -> bool {
        self.discovery
    }
    pub(crate) fn mark_test_only(&self) {
        self.test_only.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// A retained connection.test (with or without a company) or a discovery connection:
    /// held only so connection.disconnect can retry its sign-out.
    pub fn is_test_only(&self) -> bool {
        self.discovery || self.test_only.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Plain wording for a failed connect step. Codes stay unchanged for clients;
    /// the native details (already redacted) stay for support.
    fn connect_failure(&self, stage: &'static str, mut error: Diagnostic) -> Diagnostic {
        let host = self.endpoint.host_str().unwrap_or("the server");
        let code = error.code.clone();
        let plain = match (stage, code.as_str()) {
            (_, code) if code.starts_with("CERTIFICATE_") || code == "CANCELED" => None,
            (_, "TRANSPORT_FAILED") => {
                error.context.insert("host".into(), json!(host));
                Some("Herbert could not reach the server. Check the URL, VPN or network connection, and the timeout settings.".into())
            }
            ("signIn", "ACCESS_DENIED") => Some("Epicor rejected the sign-in. Check the username, password and API key for this environment.".into()),
            ("accountCheck", "ACCESS_DENIED") => Some("Epicor refused the sign-in or the account check. Check the username, password and API key, and that this account may use the Epicor REST API.".into()),
            ("session", "COMPANY_CONTEXT_UNVERIFIED") => {
                error.context.insert("company".into(), json!(self.company));
                Some("This account cannot use the chosen company. Choose a company from the list that Test connection shows.".into())
            }
            ("session", _) => Some("Epicor would not start a session for this account, for example because no license seat is free or the account is disabled or locked. Try again later or ask your Epicor administrator.".into()),
            _ => None,
        };
        if let Some(message) = plain { error.message = message; }
        error.context.insert("connectionStage".into(), json!(stage));
        error
    }
    /// The company a request addresses: the selected company, or for a discovery
    /// connection the account's default session company (empty before login).
    fn call_company(&self) -> String {
        if self.discovery {
            self.discovery_company.lock().ok().and_then(|company| company.clone()).unwrap_or_default()
        } else {
            self.company.clone()
        }
    }
    fn refuse_discovery(&self) -> Result<(), Diagnostic> {
        if self.discovery {
            return Err(Diagnostic::new("COMPANY_REQUIRED", "This connection only lists companies. Choose a company and connect before downloading."));
        }
        Ok(())
    }
    /// Companies the admitted caller can use, for connection.test. UserComp rows
    /// (with names) come from the admission read; CompList IDs are the fallback,
    /// with names from the company list; the company list alone is the last fallback.
    pub async fn available_companies(&self, cancel: &CancellationToken) -> (Vec<Value>, &'static str, Option<Diagnostic>) {
        let admitted = self.admitted_companies.lock().ok().and_then(|value| value.clone()).unwrap_or(Value::Null);
        let rows = |value: &Value| value.as_array().map(|rows| rows.iter().filter_map(|row| {
            let id = row["Company"].as_str().filter(|id| !id.is_empty())?;
            Some(json!({"id":id,"name":row["CompanyName"].as_str().or_else(|| row["Name"].as_str()).unwrap_or("")}))
        }).collect::<Vec<_>>());
        let mut companies = rows(&admitted["userComp"]).unwrap_or_default();
        let mut source = "userAccess";
        if companies.is_empty() {
            let listed = self.send("Ice.BO.CompanySvc", "GetList", json!({"whereClause":"","pageSize":0,"absolutePage":0}), false, false, true, cancel).await;
            let names = match &listed {
                Ok(response) => rows(&response.value["returnObj"]["CompanyList"]).unwrap_or_default(),
                Err(_) => vec![],
            };
            let ids = admitted["compList"].as_str().map(|text| text.split(['~', ',']).map(str::trim).filter(|id| !id.is_empty()).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
            if !ids.is_empty() {
                companies = ids.iter().map(|id| json!({"id":id,"name":names.iter().find(|row| row["id"] == id.as_str()).and_then(|row| row["name"].as_str()).unwrap_or("")})).collect();
            } else if listed.is_ok() {
                companies = names;
                source = "companyList";
            } else {
                return (vec![], "unavailable", listed.err());
            }
        }
        companies.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        companies.dedup_by(|a, b| a["id"] == b["id"]);
        (companies, source, None)
    }
    pub fn new(
        endpoint: Url,
        company: String,
        mode: AuthMode,
        credentials: Credentials,
        settings: OperatingSettings,
        budget: Arc<Budget>,
    ) -> Result<Arc<Self>, Diagnostic> {
        Self::with_options(endpoint, Some(company), mode, credentials, settings, budget, ConnectionOptions::default())
    }
    /// `company: None` creates a company-discovery connection (connection.test only).
    pub fn with_options(
        endpoint: Url,
        company: Option<String>,
        mode: AuthMode,
        credentials: Credentials,
        settings: OperatingSettings,
        budget: Arc<Budget>,
        options: ConnectionOptions,
    ) -> Result<Arc<Self>, Diagnostic> {
        settings.validate()?;
        credentials.validate(mode)?;
        let discovery = company.is_none();
        let company = company.unwrap_or_default();
        if !discovery && company.is_empty() {
            return Err(company_required());
        }
        let transport_trust = crate::transport_trust::Observed::default();
        let mut builder = Client::builder().redirect(reqwest::redirect::Policy::none());
        if options.allow_untrusted_certificate {
            // Only this connection's client relaxes issuer trust; other profiles keep defaults.
            builder = builder.use_preconfigured_tls(crate::transport_trust::relaxed_client_config(transport_trust.clone())?);
        }
        if let Some(ms) = settings.connection_timeout_ms {
            builder = builder.connect_timeout(Duration::from_millis(ms));
        }
        let client = builder.build().map_err(|_| {
            Diagnostic::new(
                "TRANSPORT_UNAVAILABLE",
                "Cannot initialize TLS and HTTP transport.",
            )
        })?;
        Ok(Arc::new(Self {
            endpoint,
            company,
            mode,
            credentials,
            client,
            budget,
            lease_budget: Budget::lease_control(),
            settings,
            lifecycle: RwLock::new(()),
            session_renewal: Mutex::new(()),
            session: Mutex::new(None),
            caller: Mutex::new(None),
            server_version: Mutex::new(None),
            allow_untrusted_certificate: options.allow_untrusted_certificate,
            transport_trust,
            discovery,
            test_only: Default::default(),
            discovery_company: std::sync::Mutex::new(None),
            admitted_companies: std::sync::Mutex::new(None),
            #[cfg(test)]
            qualification_http: std::sync::Mutex::new(Vec::new()),
        }))
    }
    async fn require_admission(&self) -> Result<(), Diagnostic> {
        if self.caller.lock().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Connect with a verified, enabled Security Manager before using Epicor.",
            ));
        }
        Ok(())
    }

    pub async fn caller(&self) -> Option<String> {
        self.caller.lock().await.clone()
    }

    /// Read only the admitted caller's group state. The full UserFile response
    /// stays in memory and is never a collection artifact or ownership record.
    pub(crate) async fn caller_access(
        &self,
        cancel: &CancellationToken,
    ) -> Result<crate::native_access::CallerAccess, Diagnostic> {
        self.ensure_session(cancel).await?;
        let caller = self.caller().await.ok_or_else(|| {
            Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before changing access.",
            )
        })?;
        let _shared = tokio::select! {
            _ = cancel.cancelled() => return Err(canceled()),
            gate = self.lifecycle.read() => gate,
        };
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        let mut native = self
            .send(
                "Ice.BO.UserFileSvc",
                "GetByID",
                json!({"userID": caller}),
                false,
                false,
                true,
                cancel,
            )
            .await?;
        let access = crate::native_access::CallerAccess::from_native(&caller, &native.value);
        native.raw.zeroize();
        access
    }

    pub(crate) async fn add_own_group(
        &self,
        group: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        crate::access::append_owned_group("", group)?;
        let caller = self.caller().await.ok_or_else(|| {
            Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before changing access.",
            )
        })?;
        self.owned_write(
            "Ice.BO.UserFileSvc",
            "AssignGroupToUsers",
            json!({"groupId": group, "userIdList": caller}),
            cancel,
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn restore_own_groups(
        &self,
        current: &crate::native_access::CallerAccess,
        groups: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        if self.caller().await.as_deref() != Some(current.caller.as_str()) {
            return Err(Diagnostic::new(
                "ACCESS_SCOPE_MISMATCH",
                "The account update must belong to the allowed to start Epicor user.",
            ));
        }
        // A sparse native update changes only the membership. Include fresh
        // identity/revision and preserve the account's password-change request.
        // Do not send password fields or a masked password back to UserFile.
        let row = json!({
            "UserID": current.caller,
            "SysRowID": current.row_id,
            "SysRevID": current.revision,
            "GroupList": groups,
            "PwdChangeRequestOn": current.password_change_request,
            "RowMod": "U",
        });
        let native = self
            .owned_write(
                "Ice.BO.UserFileSvc",
                "UpdateExt",
                json!({
                    "ds": {"UserFile": [row]},
                    "continueProcessingOnError": false,
                    "rollbackParentOnChildError": true,
                }),
                cancel,
            )
            .await?;
        crate::native_access::verify_update_result(&native.value)
    }

    /// Refresh native membership caches with a new managed session. Ordinary
    /// requests finish before the old session is released. Caller and company
    /// must still match before another collection read can be dispatched.
    pub(crate) async fn refresh_access(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let _exclusive = tokio::select! {
            _ = cancel.cancelled() => return Err(canceled()),
            gate = self.lifecycle.write() => gate,
        };
        let previous = self.caller().await.ok_or_else(|| {
            Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before refreshing access.",
            )
        })?;
        *self.caller.lock().await = None;
        if self.mode == AuthMode::Session {
            // A failed logout leaves the old session identity available for
            // reconciliation. Do not create an untracked replacement session.
            self.send(
                "Ice.Lib.SessionModSvc",
                "Logout",
                json!({}),
                false,
                true,
                true,
                cancel,
            )
            .await?;
            *self.session.lock().await = None;
            self.acquire_token(cancel).await?;
            self.login_session(cancel).await?;
        }
        // The account was checked when connecting. Epicor enforces later changes.
        *self.caller.lock().await = Some(previous);
        Ok(())
    }

    /// Private ownership operations never use automatic transport replay.
    /// Only the typed account/helper adapters can reach this method.
    pub(crate) async fn owned_write(
        &self,
        service: &str,
        method: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if !matches!(
            (service, method),
            ("Ice.BO.UserFileSvc", "AssignGroupToUsers" | "UpdateExt")
                | (
                    "Ice.Lib.EfxLibraryDesignerSvc",
                    "ImportLibrary"
                        | "ApplyChangesWithDiagnostics"
                        | "RegenerateLibrary"
                        | "PromoteToProduction"
                        | "UninstallLibrary"
                )
        ) {
            return Err(Diagnostic::new(
                "OWNED_WRITE_NOT_REVIEWED",
                "Herbert cannot make this change on the Epicor server because it has no safe way to track and undo it. Nothing was changed.",
            ));
        }
        self.ensure_session(cancel).await?;
        if self.caller().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before an owned change in Epicor.",
            ));
        }
        let _shared = tokio::select! {
            _ = cancel.cancelled() => return Err(canceled()),
            gate = self.lifecycle.read() => gate,
        };
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        // The OData path remains the ordinary native-service path. The export
        // transport class disables replay and permits transfer-sized responses.
        let result = self
            .send(service, method, arguments, true, false, true, cancel)
            .await;
        match result {
            Err(mut error) if service == "Ice.Lib.EfxLibraryDesignerSvc" => {
                // Compiler diagnostics explain failures in the reviewed helper.
                // Other server text stays private. Scrub connection secrets even
                // from this narrow projection before it reaches an interface.
                let mut messages = Vec::new();
                if let Some(details) = &error.native_details {
                    collect_compiler_messages(details, &mut messages);
                }
                if !messages.is_empty() {
                    let message = self.redact_server_text(messages.join("\n")).await;
                    error.code = "HELPER_COMPILATION_FAILED".into();
                    error.message = "Epicor could not compile Herbert's temporary server helper, so the content that needs it was not collected. The redacted compiler output is in the technical details. Check that this Epicor version is supported.".into();
                    // Redacted server compiler output: technical detail, never the message.
                    error.context.insert("serverMessage".into(), json!(message));
                    error.retryable = false;
                }
                Err(error)
            }
            result => result,
        }
    }

    /// Scrub this connection's secrets (password, API key, Basic header, session
    /// token and ID) from server text before it becomes `context.serverMessage`.
    pub(crate) async fn redact_server_text(&self, mut message: String) -> String {
        let secrets = server_secrets(&self.credentials, self.session.lock().await.as_ref());
        scrub_text(&mut message, &secrets);
        message
    }
    /// The same scrub applied to every string inside a JSON value, keeping its structure.
    pub(crate) async fn redact_server_value(&self, value: &mut Value) {
        let secrets = server_secrets(&self.credentials, self.session.lock().await.as_ref());
        scrub_value(value, &secrets);
    }

    /// Private native-policy reads. Guests cannot use this route to inspect
    /// arbitrary accounts or obtain a general business-service capability.
    pub(crate) async fn security_read(
        &self,
        service: &str,
        method: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if !matches!(
            (service, method),
            ("Ice.BO.ObjectSecuritySvc", "GetByIDEx") | ("Ice.BO.SecGroupSvc", "GetByID")
        ) {
            return Err(Diagnostic::new(
                "POLICY_READ_NOT_REVIEWED",
                "Herbert does not support reading this Epicor security policy.",
            ));
        }
        self.ensure_session(cancel).await?;
        if self.caller().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before inspecting Epicor access policy.",
            ));
        }
        let _shared = tokio::select! { _ = cancel.cancelled() => return Err(canceled()), gate = self.lifecycle.read() => gate };
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        self.send(service, method, arguments, false, false, true, cancel)
            .await
    }

    pub(crate) async fn canonical_group(
        &self,
        group: &str,
        cancel: &CancellationToken,
    ) -> Result<String, Diagnostic> {
        crate::access::append_owned_group("", group)?;
        let response = self
            .security_read(
                "Ice.BO.SecGroupSvc",
                "GetByID",
                json!({"secGroupCode": group}),
                cancel,
            )
            .await?;
        let rows = response.value["returnObj"]["SecGroup"]
            .as_array()
            .filter(|rows| rows.len() == 1)
            .ok_or_else(|| {
                Diagnostic::new(
                    "ACCESS_GROUP_UNVERIFIED",
                    "Epicor readback did not identify one exact security group.",
                )
            })?;
        let row = &rows[0];
        let id = row["SecGroupCode"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                Diagnostic::new(
                    "ACCESS_GROUP_UNVERIFIED",
                    "The Epicor group omitted its canonical identity.",
                )
            })?;
        if row["Company"]
            .as_str()
            .is_none_or(|company| !company.is_empty() && company != self.company)
            || row["SysRowID"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .is_none()
        {
            return Err(Diagnostic::new(
                "ACCESS_GROUP_UNVERIFIED",
                "The group does not have Epicor ID in the selected company.",
            ));
        }
        Ok(id.into())
    }

    pub(crate) async fn helper_metadata(
        &self,
        method: &str,
        arguments: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if !matches!(method, "GetDefaults" | "IsValidLibraryId") {
            return Err(Diagnostic::new(
                "HELPER_METADATA_NOT_REVIEWED",
                "A plugin asked for helper information that Herbert does not allow. Nothing was changed on the server. Update the plugin to the version that ships with this Herbert release.",
            ));
        }
        self.ensure_session(cancel).await?;
        if self.caller().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the Epicor user before inspecting helper metadata.",
            ));
        }
        let _shared = tokio::select! {
            _ = cancel.cancelled() => return Err(canceled()),
            gate = self.lifecycle.read() => gate,
        };
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        self.send(
            "Ice.Lib.EfxLibraryDesignerSvc",
            method,
            arguments,
            false,
            false,
            true,
            cancel,
        )
        .await
    }
    pub async fn server_version(&self) -> Option<String> { self.server_version.lock().await.clone() }
    pub async fn connect(&self, cancel: &CancellationToken) -> Result<Value, Diagnostic> {
        let _gate = self.lifecycle.write().await;
        // A failed reconnect must revoke any earlier admission.
        *self.caller.lock().await = None;
        if self.mode == AuthMode::Session {
            self.acquire_token(cancel).await.map_err(|error| self.connect_failure("signIn", error))?;
            // Native service reads need SessionInfo, even for the initial role check.
            self.login_session(cancel).await.map_err(|error| self.connect_failure("session", error))?;
        }
        if self.discovery {
            let current = self.send("Ice.Lib.SessionModSvc", "GetCurrentValuesFromServer", json!({}), false, true, true, cancel).await?.value;
            let company = current.pointer("/parameters/companyID").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| {
                Diagnostic::new("COMPANY_DISCOVERY_UNAVAILABLE", "Epicor did not report a default company for this account, so Herbert cannot list its companies. Enter the company ID manually.")
            })?;
            if let Ok(mut path) = self.discovery_company.lock() {
                *path = Some(company.to_owned());
            }
        }
        let caller = self.verify_admission(cancel).await.map_err(|error| self.connect_failure("accountCheck", error))?;
        let version = self
            .send(
                "Ice.Lib.SessionModSvc",
                "GetDatabaseVersion",
                json!({}),
                false,
                true,
                true,
                cancel,
            )
            .await?
            .value;
        *self.server_version.lock().await = observed_database_version(&version);
        *self.caller.lock().await = Some(caller.clone());
        Ok(
            json!({"endpoint":self.endpoint.as_str(),"company":self.call_company(),"caller":caller,"authMode":self.mode,"admission":"verifiedSecurityManager","version":version.get("returnObj")}),
        )
    }
    async fn acquire_token(&self, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        let url = self
            .endpoint
            .join("api/TokenResource")
            .map_err(|_| Diagnostic::new("INVALID_ENDPOINT", "Cannot resolve token route."))?;
        let mut request = self
            .client
            .post(url)
            .basic_auth(&self.credentials.username, Some(&self.credentials.password))
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            // HTTP.sys and other strict frontends require framing even for an empty POST.
            .header(reqwest::header::CONTENT_LENGTH, "0")
            .body("");
        if let Some(key) = &self.credentials.api_key {
            request = request.header("X-API-Key", key);
        }
        let value = self.consume_once_on(request, false, cancel, &mut None, &self.lease_budget).await?.value;
        if value.get("TokenType").and_then(Value::as_str) != Some("Bearer") {
            return Err(Diagnostic::new(
                "TOKEN_TYPE_INVALID",
                "The Epicor token endpoint did not confirm a Bearer token.",
            ));
        }
        let token = value
            .get("AccessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Diagnostic::new(
                    "AUTHENTICATION_FAILED",
                    "The token endpoint did not return an access token.",
                )
            })?;
        let seconds = value
            .get("ExpiresIn")
            .and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            })
            .filter(|seconds| *seconds > 0)
            .ok_or_else(|| {
                Diagnostic::new(
                    "TOKEN_LIFETIME_UNKNOWN",
                    "The token endpoint did not return a valid lifetime. Herbert cannot safely schedule session renewal.",
                )
            })?;
        let expires = Instant::now()
            .checked_add(Duration::from_secs(seconds))
            .ok_or_else(|| {
                Diagnostic::new(
                    "TOKEN_LIFETIME_INVALID",
                    "The server supplied an unusable token lifetime.",
                )
            })?;
        let old = self.session.lock().await.take();
        let id = old.and_then(|s| s.id);
        *self.session.lock().await = Some(Session {
            token: Zeroizing::new(token.into()),
            expires,
            id,
        });
        Ok(())
    }
    async fn login_session(&self, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        let response = self
            .send(
                "Ice.Lib.SessionModSvc",
                "Login",
                json!({}),
                false,
                true,
                false,
                cancel,
            )
            .await?
            .value;
        let id = response
            .get("returnObj")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Diagnostic::new(
                    "SESSION_ESTABLISHMENT_FAILED",
                    "Epicor Login did not return a SessionMod identity.",
                )
            })?;
        if let Some(s) = self.session.lock().await.as_mut() {
            s.id = Some(Zeroizing::new(id.into()));
        }
        if self.discovery {
            // Company discovery keeps the account's default session company.
            return Ok(());
        }
        self.send(
            "Ice.Lib.SessionModSvc",
            "SetCompany",
            json!({"newCompany":self.company}),
            false,
            true,
            true,
            cancel,
        )
        .await?;
        self.check_company(cancel).await
    }
    async fn check_company(&self, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        let response = self
            .send(
                "Ice.Lib.SessionModSvc",
                "GetCurrentValuesFromServer",
                json!({}),
                false,
                true,
                true,
                cancel,
            )
            .await?
            .value;
        if response
            .get("parameters")
            .and_then(|p| p.get("companyID"))
            .and_then(Value::as_str)
            != Some(self.company.as_str())
        {
            return Err(Diagnostic::new(
                "COMPANY_CONTEXT_UNVERIFIED",
                "The Epicor session did not confirm the selected company.",
            ));
        }
        Ok(())
    }
    async fn verify_admission(&self, cancel: &CancellationToken) -> Result<String, Diagnostic> {
        let current = self
            .send(
                "Ice.BO.UserFileSvc",
                "GetUserFile",
                json!({}),
                false,
                false,
                true,
                cancel,
            )
            .await?
            .value;
        let users = current
            .get("returnObj")
            .and_then(|r| r.get("UserFile"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Diagnostic::new(
                    "ADMISSION_UNKNOWN",
                    "The Epicor current-user read did not establish Epicor user or role.",
                )
            })?;
        if users.len() != 1 {
            return Err(Diagnostic::new(
                "ADMISSION_UNKNOWN",
                "The Epicor current-user read returned an ambiguous Epicor user.",
            ));
        }
        let user = &users[0];
        let caller = user
            .get("DcdUserID")
            .or_else(|| user.get("UserID"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Diagnostic::new(
                    "ADMISSION_UNKNOWN",
                    "The Epicor current-user identity is missing.",
                )
            })?;
        if user.get("SecurityMgr").and_then(Value::as_bool) != Some(true) {
            return Err(Diagnostic::new(
                "SECURITY_MANAGER_REQUIRED",
                "This Epicor user is not a verified Security Manager. Ask your Epicor administrator to verify the selected account’s Security Manager access, then test again. Collection is refused.",
            ));
        }
        if user.get("UserDisabled").and_then(Value::as_bool) != Some(false)
            || user.get("LockedOut").and_then(Value::as_bool) != Some(false)
        {
            return Err(Diagnostic::new(
                "CALLER_DISABLED_OR_UNKNOWN",
                "The Epicor user must be enabled and unlocked. Ask your Epicor administrator to verify both states, then test again.",
            ));
        }
        if !caller.eq_ignore_ascii_case(&self.credentials.username) {
            return Err(Diagnostic::new(
                "CALLER_MISMATCH",
                "The Epicor current-user identity does not match the authenticated credentials.",
            ));
        }
        // Keep only the accessible-company facts for connection.test; the rest
        // of the account row stays in memory for this check only.
        let companies = json!({"userComp":current["returnObj"]["UserComp"],"compList":user["CompList"]});
        if let Ok(mut admitted) = self.admitted_companies.lock() {
            *admitted = Some(companies);
        }
        Ok(caller.into())
    }
    async fn ensure_session(&self, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        self.require_admission().await?;
        if self.mode != AuthMode::Session {
            return Ok(());
        }
        let due = self
            .session
            .lock()
            .await
            .as_ref()
            .is_none_or(|s| Instant::now() >= s.expires);
        if !due {
            return Ok(());
        }
        // Refresh only the token for the existing session. In-flight requests
        // already hold their headers; they need not finish before a heartbeat.
        // The read guard still excludes reconnect and logout.
        let _renewal = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.session_renewal.lock()=>gate};
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        if self
            .session
            .lock()
            .await
            .as_ref()
            .is_some_and(|s| Instant::now() < s.expires)
        {
            return Ok(());
        }
        let previous = self.caller().await;
        // A failed renewal leaves the connection unavailable. Renewing the same
        // credentials does not need another account-role lookup.
        *self.caller.lock().await = None;
        self.acquire_token(cancel).await?;
        self.check_company(cancel).await?;
        *self.caller.lock().await = previous;
        Ok(())
    }
    pub async fn native_entity_page(
        &self,
        service: &str,
        entity: &str,
        query: &std::collections::BTreeMap<String, String>,
        page_size: u32,
        page: u64,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        let url = entity_page_url(
            &self.endpoint,
            &self.company,
            service,
            entity,
            query,
            page_size,
            page,
        )?;
        if self.caller.lock().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify Epicor user start check before reading definitions.",
            ));
        }
        self.ensure_session(cancel).await?;
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        let request = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .header("callSettings", json!({"Company":self.call_company()}).to_string());
        self.consume(self.authenticate(request, true).await?, false, true, cancel)
            .await
    }
    pub async fn native_read(
        &self,
        service: &str,
        method: &str,
        args: Value,
        export: bool,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if self.caller.lock().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify Epicor user start check before reading definitions.",
            ));
        }
        self.ensure_session(cancel).await?;
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        self.send(
            service,
            method,
            args,
            export,
            service == "Ice.Lib.SessionModSvc",
            true,
            cancel,
        )
        .await
    }
    pub async fn logout(&self, cancel: &CancellationToken) -> Result<(), Diagnostic> {
        let _exclusive = self.lifecycle.write().await;
        let managed = self.mode == AuthMode::Session
            && self
                .session
                .lock()
                .await
                .as_ref()
                .is_some_and(|s| s.id.is_some());
        let result = if managed {
            self.send(
                "Ice.Lib.SessionModSvc",
                "Logout",
                json!({}),
                false,
                true,
                true,
                cancel,
            )
            .await
            .map(|_| ())
        } else {
            Ok(())
        };
        result.map_err(|_| {
            Diagnostic::new(
                "LOGOUT_UNCERTAIN",
                "Epicor SessionMod logout was not confirmed. Herbert retains this connection. Call connection.disconnect to retry logout.",
            )
        })?;
        *self.session.lock().await = None;
        *self.caller.lock().await = None;
        Ok(())
    }
    async fn send(
        &self,
        service: &str,
        method: &str,
        args: Value,
        export: bool,
        lifecycle: bool,
        include_session: bool,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        if !valid_segment(service) || !valid_segment(method) {
            return Err(Diagnostic::new(
                "INVALID_NATIVE_ROUTE",
                "The Epicor service or method identity is invalid.",
            ));
        }
        let mut url = self.endpoint.clone();
        {
            let mut parts = url.path_segments_mut().map_err(|_| {
                Diagnostic::new("INVALID_ENDPOINT", "Cannot form an Epicor endpoint.")
            })?;
            parts.pop_if_empty().push("api").push("v2");
            if !lifecycle {
                parts.push("odata").push(&self.call_company());
            }
            parts.push(service).push(method);
        }
        let mut request = self
            .client
            .post(url)
            .header("Accept", "application/json")
            .header("callSettings", json!({"Company":self.call_company()}).to_string())
            .json(&args);
        request = self.authenticate(request, include_session).await?;
        if lifecycle {
            self.consume_once_on(request, export, cancel, &mut None, &self.lease_budget).await
        } else {
            self.consume(request, export, !export, cancel).await
        }.map_err(|mut error| {
            error.context.insert("service".into(), json!(service));
            error.context.insert("method".into(), json!(method));
            error
        })
    }
    async fn authenticate(
        &self,
        mut request: RequestBuilder,
        include_session: bool,
    ) -> Result<RequestBuilder, Diagnostic> {
        match self.mode {
            AuthMode::BasicApiKey => {
                request = request
                    .basic_auth(&self.credentials.username, Some(&self.credentials.password));
            }
            AuthMode::Session => {
                let session = self.session.lock().await;
                let s = session.as_ref().ok_or_else(|| {
                    Diagnostic::new("SESSION_REQUIRED", "Establish a managed session first.")
                })?;
                request = request.bearer_auth(s.token.as_str());
                if include_session {
                    if let Some(id) = &s.id {
                        request = request
                            .header("SessionInfo", json!({"SessionID":id.as_str()}).to_string());
                    }
                }
            }
        }
        if let Some(key) = &self.credentials.api_key {
            request = request.header("X-API-Key", key);
        }
        Ok(request)
    }
    pub(crate) async fn helper_call(
        &self,
        helper: &crate::helpers::VerifiedHelper,
        args: Value,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        self.refuse_discovery()?;
        if helper.company != self.company
            || args.get(&helper.company_argument).and_then(Value::as_str)
                != Some(self.company.as_str())
            || !valid_segment(&helper.library)
            || !valid_segment(&helper.function)
        {
            return Err(Diagnostic::new(
                "HELPER_SCOPE_MISMATCH",
                "The helper call must use the fixed selected company and verified Epicor ID.",
            ));
        }
        if self.caller.lock().await.is_none() {
            return Err(Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify Epicor user start check before reading definitions.",
            ));
        }
        self.ensure_session(cancel).await?;
        let _shared = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),gate=self.lifecycle.read()=>gate};
        // Recheck under the lifecycle lock after concurrent renewal or logout.
        self.require_admission().await?;
        let mut url = self.endpoint.clone();
        {
            let mut parts = url.path_segments_mut().map_err(|_| {
                Diagnostic::new(
                    "INVALID_ENDPOINT",
                    "Cannot form the Epicor Function endpoint.",
                )
            })?;
            parts
                .pop_if_empty()
                .push("api")
                .push("v2")
                .push("efx")
                .push(&self.company)
                .push(&helper.library)
                .push(&helper.function);
        }
        let request = self
            .client
            .post(url)
            .header("Accept", "application/json")
            .header("callSettings", json!({"Company":self.call_company()}).to_string())
            .json(&args);
        let request = self.authenticate(request, true).await?;
        // Owned temporary-state calls reconcile their retained IDs explicitly.
        // A transport failure must never automatically replay preparation or close.
        self.consume(
            request,
            true,
            helper.effect == herbert_contract::HelperEffect::Read,
            cancel,
        )
        .await
    }
    async fn consume(
        &self,
        request: RequestBuilder,
        export: bool,
        replay_safe: bool,
        cancel: &CancellationToken,
    ) -> Result<NativeResponse, Diagnostic> {
        let mut attempt = 0u32;
        loop {
            let fresh = request.try_clone().ok_or_else(|| {
                Diagnostic::new(
                    "REQUEST_NOT_REPLAYABLE",
                    "The request body cannot be cloned for a fresh Epicor read.",
                )
            })?;
            let mut retry_after = None;
            let attempt_started = Instant::now();
            match self
                .consume_once(fresh, export, cancel, &mut retry_after)
                .await
            {
                Ok(value) => {
                    crate::activity::record_transport(attempt + 1,
                        attempt_started.elapsed().as_millis(), None, None);
                    return Ok(value);
                }
                Err(mut error) => {
                    let transient = error.retryable
                        || matches!(
                            error.code.as_str(),
                            "TRANSPORT_FAILED" | "TRANSFER_STALLED" | "TRANSFER_TRUNCATED"
                        );
                    error.retryable = transient && replay_safe;
                    if !replay_safe
                        || !transient
                        || attempt >= self.settings.retry_attempts
                        || cancel.is_cancelled()
                    {
                        crate::activity::record_transport(attempt + 1,
                            attempt_started.elapsed().as_millis(), Some(&error.code), None);
                        return Err(error);
                    }
                    attempt += 1;
                    let delay = retry_after.unwrap_or(Duration::ZERO).max(
                        self.settings
                            .retry_backoff_ms
                            .map(Duration::from_millis)
                            .unwrap_or(Duration::ZERO),
                    );
                    crate::activity::record_transport(attempt,
                        attempt_started.elapsed().as_millis(), Some(&error.code), Some(delay.as_millis()));
                    tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),_ = tokio::time::sleep_until(tokio::time::Instant::now().checked_add(delay).ok_or_else(||Diagnostic::new("TIMING_NOT_REPRESENTABLE","The server or configured retry delay exceeds this platform\'s scheduler clock representation."))?)=>{}}
                }
            }
        }
    }
    async fn consume_once(
        &self,
        request: RequestBuilder,
        export: bool,
        cancel: &CancellationToken,
        retry_after: &mut Option<Duration>,
    ) -> Result<NativeResponse, Diagnostic> {
        self.consume_once_on(request, export, cancel, retry_after, &self.budget).await
    }
    async fn consume_once_on(
        &self, mut request: RequestBuilder, export: bool,
        cancel: &CancellationToken, retry_after: &mut Option<Duration>, budget: &Arc<Budget>,
    ) -> Result<NativeResponse, Diagnostic> {
        let _slot = budget.acquire(cancel).await?;
        let start = Instant::now();
        if let Some(ms) = if export {
            self.settings.export_timeout_ms
        } else {
            self.settings.service_timeout_ms
        } {
            request = request.timeout(Duration::from_millis(ms));
        }
        #[cfg(test)]
        let trace_path = request.try_clone().and_then(|r| r.build().ok()).map(|r| r.url().path().to_owned()).unwrap_or_default();
        let mut response = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),r=request.send()=>r.map_err(|error|self.request_error(&error))?};
        let status = response.status();
        let length = response.content_length();
        let mut raw = Vec::new();
        loop {
            let next = async {
                if let Some(ms) = self.settings.inactivity_timeout_ms {
                    tokio::time::timeout(Duration::from_millis(ms), response.chunk())
                        .await
                        .map_err(|_| {
                            Diagnostic::new(
                                "TRANSFER_STALLED",
                                "The response exceeded the configured inactivity timeout.",
                            )
                        })?
                        .map_err(|_| transport_error())
                } else {
                    response.chunk().await.map_err(|_| transport_error())
                }
            };
            let chunk = tokio::select! {_ = cancel.cancelled()=>return Err(canceled()),r=next=>r?};
            match chunk {
                Some(c) => raw.extend_from_slice(&c),
                None => break,
            }
        }
        #[cfg(test)]
        self.qualification_http.lock().unwrap().push(json!({"path":trace_path,"status":status.as_u16(),"responseBytes":raw.len(),"elapsedMicros":start.elapsed().as_micros()}));
        if length.is_some_and(|n| n != raw.len() as u64) {
            return Err(Diagnostic::new(
                "TRANSFER_TRUNCATED",
                "The Epicor response length differs from Content-Length.",
            ));
        }
        if !status.is_success() {
            if let Some(value) = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
            {
                *retry_after = if let Ok(seconds) = value.trim().parse::<u64>() {
                    Some(Duration::from_secs(seconds))
                } else {
                    httpdate::parse_http_date(value).ok().map(|date| {
                        date.duration_since(std::time::SystemTime::now())
                            .unwrap_or(Duration::ZERO)
                    })
                };
            }
            let mut diagnostic = match status {
                StatusCode::UNAUTHORIZED => Diagnostic::new(
                    "ACCESS_DENIED",
                    "Epicor authentication or object access was refused.",
                ),
                StatusCode::FORBIDDEN => {
                    Diagnostic::new("ACCESS_DENIED", "Epicor authorization was refused.")
                }
                StatusCode::NOT_FOUND => Diagnostic::new(
                    "NOT_FOUND",
                    "The exact Epicor object or capability was not found.",
                ),
                StatusCode::TOO_MANY_REQUESTS => Diagnostic::new(
                    "THROTTLED",
                    "The server requested throttling; retry policy must honor its response timing.",
                ),
                _ => Diagnostic::new(
                    "NATIVE_REFUSAL",
                    format!(
                        "Epicor operation returned HTTP {}. Server text was omitted to avoid credential reflection.",
                        status.as_u16()
                    ),
                ),
            };
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                if let Some(duration) = retry_after {
                    self.budget.throttle(*duration)?;
                }
            }
            diagnostic.retryable = matches!(
                status,
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            );
            if diagnostic.retryable && status != StatusCode::TOO_MANY_REQUESTS {
                diagnostic.code = "SERVICE_UNAVAILABLE".into();
            }
            diagnostic.context.insert("httpStatus".into(), json!(status.as_u16()));
            if let Some(parts)=response.url().path_segments() {
                let mut parts=parts.rev();
                if let Some(method)=parts.next(){diagnostic.context.insert("method".into(),json!(method));}
                if let Some(service)=parts.next(){diagnostic.context.insert("service".into(),json!(service));}
            }
            if let Some(id)=safe_error_correlation_id(&raw,&self.credentials){diagnostic.context.insert("correlationId".into(),json!(id));}
            let content_type=response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).and_then(|v|v.split(';').next()).unwrap_or("").trim().to_ascii_lowercase();
            diagnostic.context.insert("responseContentType".into(),json!(match content_type.as_str(){"application/json"=>"application/json","text/plain"=>"text/plain","text/html"=>"text/html","application/problem+json"=>"application/problem+json",_=>"other"}));
            diagnostic.native_details = serde_json::from_slice(&raw).ok();
            // Epicor can echo a rejected API key. Remove connection secrets before
            // diagnostic details reach a caller, a checkpoint, or a test failure.
            if let Some(details) = &mut diagnostic.native_details {
                redact_connection_secrets(details, &self.credentials);
            }
            raw.zeroize();
            return Err(diagnostic);
        }
        let value = serde_json::from_slice(&raw).map_err(|_| {
            Diagnostic::new(
                "INVALID_NATIVE_RESPONSE",
                "The server returned malformed JSON; response contents were omitted.",
            )
        })?;
        Ok(NativeResponse {
            raw,
            value,
            elapsed_ms: start.elapsed().as_millis(),
        })
    }
}

/// Every secret this connection could see reflected in server text: password, API key,
/// Basic header and, while signed in, the session token and ID. Empty values are dropped.
fn server_secrets(credentials: &Credentials, session: Option<&Session>) -> Vec<Zeroizing<String>> {
    use base64::Engine as _;
    let mut secrets = vec![
        Zeroizing::new(credentials.password.clone()),
        Zeroizing::new(credentials.api_key.clone().unwrap_or_default()),
        Zeroizing::new(base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", credentials.username, credentials.password))),
    ];
    if let Some(session) = session {
        secrets.push(Zeroizing::new(session.token.as_str().to_owned()));
        if let Some(id) = &session.id {
            secrets.push(Zeroizing::new(id.as_str().to_owned()));
        }
    }
    secrets.retain(|secret| !secret.is_empty());
    secrets
}
fn scrub_text(text: &mut String, secrets: &[Zeroizing<String>]) {
    for secret in secrets {
        if text.contains(secret.as_str()) {
            *text = text.replace(secret.as_str(), "[redacted]");
        }
    }
}
fn scrub_value(value: &mut Value, secrets: &[Zeroizing<String>]) {
    match value {
        Value::String(text) => scrub_text(text, secrets),
        Value::Array(items) => items.iter_mut().for_each(|item| scrub_value(item, secrets)),
        Value::Object(items) => items.values_mut().for_each(|item| scrub_value(item, secrets)),
        _ => {}
    }
}

#[test]
fn server_text_and_values_are_scrubbed_of_every_connection_secret() {
    use base64::Engine as _;
    let credentials = Credentials {
        username: "fixture".into(),
        password: "fixture-password".into(),
        api_key: Some("fixture-key".into()),
    };
    let session = Session {
        token: Zeroizing::new("fixture-token".into()),
        expires: Instant::now(),
        id: Some(Zeroizing::new("fixture-session".into())),
    };
    let secrets = server_secrets(&credentials, Some(&session));
    let basic = base64::engine::general_purpose::STANDARD.encode("fixture:fixture-password");
    let mut text = format!("pw fixture-password key fixture-key basic {basic} token fixture-token id fixture-session user fixture");
    scrub_text(&mut text, &secrets);
    assert_eq!(text, "pw [redacted] key [redacted] basic [redacted] token [redacted] id [redacted] user fixture");
    // Per JSON value: structure, numbers and booleans are kept; every string is scrubbed.
    let mut value = serde_json::json!({"SameIDInCurrentCompany":"true","log":["ok","Session fixture-token expired"],
        "nested":{"m":"bad fixture-key"},"n":3,"b":true});
    scrub_value(&mut value, &secrets);
    assert_eq!(value, serde_json::json!({"SameIDInCurrentCompany":"true","log":["ok","Session [redacted] expired"],
        "nested":{"m":"bad [redacted]"},"n":3,"b":true}));
    // Signed out: no session secrets; an absent API key adds no empty pattern.
    let anonymous = Credentials { username: "fixture".into(), password: "fixture-password".into(), api_key: None };
    assert_eq!(server_secrets(&anonymous, None).len(), 2);
}

fn redact_connection_secrets(value: &mut Value, credentials: &Credentials) {
    use base64::Engine as _;
    let basic = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", credentials.username, credentials.password));
    let secrets = [
        credentials.password.as_str(),
        credentials.api_key.as_deref().unwrap_or(""),
        basic.as_str(),
    ];
    fn redact(value: &mut Value, secrets: &[&str]) {
        match value {
            Value::String(text) => {
                for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                    *text = text.replace(secret, "[redacted]");
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| redact(item, secrets)),
            Value::Object(items) => {
                for item in items.values_mut() {
                    redact(item, secrets);
                }
            }
            _ => {}
        }
    }
    redact(value, &secrets);
}

#[test]
fn native_error_details_do_not_reflect_connection_secrets() {
    let credentials = Credentials {
        username: "fixture".into(),
        password: "fixture-password".into(),
        api_key: Some("fixture-key".into()),
    };
    let mut details = serde_json::json!({
        "ErrorMessage": "Invalid API Key fixture-key.",
        "nested": [{"message": "Password fixture-password rejected."}],
        "HttpStatus": 401
    });
    redact_connection_secrets(&mut details, &credentials);
    assert_eq!(details["ErrorMessage"], "Invalid API Key [redacted].");
    assert_eq!(
        details["nested"][0]["message"],
        "Password [redacted] rejected."
    );
    assert_eq!(details["HttpStatus"], 401);
}
fn entity_page_url(
    endpoint: &Url,
    company: &str,
    service: &str,
    entity: &str,
    query: &std::collections::BTreeMap<String, String>,
    page_size: u32,
    page: u64,
) -> Result<Url, Diagnostic> {
    if !valid_segment(service) || !valid_segment(entity) || page_size == 0 || page == 0 {
        return Err(Diagnostic::new(
            "INVALID_ENTITY_READ",
            "Supply a positive Epicor schema page and page size.",
        ));
    }
    let skip = (page - 1)
        .checked_mul(u64::from(page_size))
        .ok_or_else(|| {
            Diagnostic::new(
                "NATIVE_PAGING_RANGE",
                "The requested Epicor page offset exceeds UInt64.",
            )
        })?;
    let mut url = endpoint.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|_| {
            Diagnostic::new(
                "INVALID_ENDPOINT",
                "Cannot form the Epicor schema endpoint.",
            )
        })?
        .pop_if_empty()
        .push("api")
        .push("v2")
        .push("odata")
        .push(company)
        .push(service)
        .push(entity);
    for (key, value) in query {
        if key != "$top" && key != "$skip" {
            url.query_pairs_mut().append_pair(key, value);
        }
    }
    url.query_pairs_mut()
        .append_pair("$top", &page_size.to_string())
        .append_pair("$skip", &skip.to_string());
    Ok(url)
}
/// Delay before retry `attempt` (0-based) after a leaseBusy reply: 10 ms doubling to a 1 s
/// ceiling, each drawn uniformly from the upper half of that step (`jitter` 0..=u16::MAX).
/// The registry writer holds the lock for one lease request (milliseconds), so 10 ms keeps the
/// uncontended case fast. The 1 s ceiling keeps a busy retry far inside the lease timing: a
/// renewal waiting on the registry still lands well within the 15 s margin before a lease is
/// treated as lost (server_leases MARGIN), while contending runs poll at most about once a second.
fn lease_busy_delay(attempt: u32, jitter: u16) -> Duration {
    const FIRST_MS: u64 = 10;
    const CEILING_MS: u64 = 1_000;
    let step = FIRST_MS.saturating_mul(1u64 << attempt.min(16)).min(CEILING_MS);
    Duration::from_millis(step / 2 + step / 2 * u64::from(jitter) / u64::from(u16::MAX))
}

#[cfg(test)]
mod lease_busy_tests {
    use super::*;

    #[test]
    fn lease_busy_backoff_grows_with_jitter_up_to_its_ceiling_and_never_stops() {
        let ms = |attempt, jitter| lease_busy_delay(attempt, jitter).as_millis();
        assert_eq!((ms(0, 0), ms(0, u16::MAX)), (5, 10));
        assert_eq!((ms(3, 0), ms(3, u16::MAX)), (40, 80));
        for attempt in [7, 8, 20, 1_000, u32::MAX] {
            assert_eq!((ms(attempt, 0), ms(attempt, u16::MAX)), (500, 1_000), "attempt {attempt}");
        }
        let spread = (0..=u16::MAX).step_by(4_096).map(|jitter| ms(5, jitter)).collect::<std::collections::BTreeSet<_>>();
        assert!(spread.len() > 8, "jitter spreads contending runs: {spread:?}");
        let mut previous = 0;
        for attempt in 0..12 {
            let ceiling = ms(attempt, u16::MAX);
            assert!(ceiling >= previous && ceiling <= 1_000);
            previous = ceiling;
        }
    }
}

#[cfg(test)]
mod entity_read_tests {
    use super::*;
    #[test]
    fn entity_page_uses_plugin_route_projection_and_encoded_company() {
        let endpoint =
            crate::input::endpoint("https://example.test/Kinetic/ApPs/ErP/Home?company=X#screen")
                .unwrap();
        let url = entity_page_url(
            &endpoint,
            "A/B",
            "Vendor.BO.CustomSvc",
            "CustomEntities",
            &std::collections::BTreeMap::from([("$select".into(), "CustomID".into())]),
            200,
            3,
        )
        .unwrap();
        assert_eq!(
            url.path(),
            "/Kinetic/api/v2/odata/A%2FB/Vendor.BO.CustomSvc/CustomEntities"
        );
        let pairs = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(pairs["$skip"], "400");
        assert_eq!(pairs["$top"], "200");
        assert_eq!(pairs["$select"], "CustomID");
        assert!(!pairs.contains_key("$orderby"));
        assert!(
            entity_page_url(
                &endpoint,
                "C",
                "Vendor.BO.CustomSvc",
                "CustomEntities",
                &Default::default(),
                0,
                1
            )
            .is_err()
        );
        assert!(
            entity_page_url(
                &endpoint,
                "C",
                "Vendor.BO.CustomSvc",
                "CustomEntities",
                &Default::default(),
                1,
                0
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "connection_admission_tests.rs"]
mod admission_tests;

fn observed_database_version(response:&Value)->Option<String>{
    let value=response.get("returnObj")?;
    if let Some(text)=value.as_str(){return (!text.is_empty()&&text.split('.').all(|p|!p.is_empty()&&p.parse::<u64>().is_ok())).then(||text.to_owned());}
    ["_Major","_Minor","_Build","_Revision"].into_iter().map(|key|value[key].as_u64().map(|n|n.to_string())).collect::<Option<Vec<_>>>().map(|parts|parts.join("."))
}
#[cfg(test)]
mod support_version_tests {
    use super::*;
    #[test]
    fn native_database_version_struct_and_legacy_string_are_safe(){
        assert_eq!(observed_database_version(&json!({"returnObj":{"_Major":12,"_Minor":2,"_Build":100,"_Revision":9}})).as_deref(),Some("12.2.100.9"));
        assert_eq!(observed_database_version(&json!({"returnObj":"12.1.100.29"})).as_deref(),Some("12.1.100.29"));
        assert!(observed_database_version(&json!({"returnObj":"private-server-name"})).is_none());
        assert!(observed_database_version(&json!({"returnObj":{"_Major":12}})).is_none());
    }
}

fn safe_error_correlation_id(raw:&[u8],credentials:&Credentials)->Option<String>{
    let text=std::str::from_utf8(raw).ok()?;
    let lower=text.to_ascii_lowercase();let marker="correlation id:";
    let start=lower.rfind(marker)?+marker.len();
    let candidate=text.get(start..)?.trim_start().get(..36)?;
    let id=uuid::Uuid::parse_str(candidate).ok()?.hyphenated().to_string();
    let compact=id.replace('-',"");
    if [Some(credentials.username.as_str()),Some(credentials.password.as_str()),credentials.api_key.as_deref()].into_iter().flatten().filter(|s|!s.is_empty()).any(|s|s.to_ascii_lowercase().replace('-',"").contains(&compact)){return None;}
    Some(id)
}
#[cfg(test)]
mod support_correlation_tests {
    use super::*;
    #[test]
    fn server_trace_id_survives_but_credential_reflection_does_not(){
        let id="7f6e4f3f-bd79-4b76-b9a8-f3866aedc226";
        let body=format!("Untrusted business text. Correlation ID: {id}");
        let mut credentials=Credentials{username:"user".into(),password:"private".into(),api_key:None};
        assert_eq!(safe_error_correlation_id(body.as_bytes(),&credentials).as_deref(),Some(id));
        credentials.api_key=Some(id.to_uppercase());assert!(safe_error_correlation_id(body.as_bytes(),&credentials).is_none());
        credentials.api_key=Some(id.replace('-',""));assert!(safe_error_correlation_id(body.as_bytes(),&credentials).is_none());
        assert!(safe_error_correlation_id(b"Correlation ID: confidential-product",&credentials).is_none());
    }
}
