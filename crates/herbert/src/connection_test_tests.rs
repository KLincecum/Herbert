//! connection.test with company discovery, through the shared Host operations.
use super::*;
use crate::test_http::Server;
use std::sync::Mutex as StdMutex;

/// Synthetic managed-session server. `options` toggles failures and response shapes.
fn server(options: Value) -> (Server, Arc<StdMutex<Value>>) {
    let state = Arc::new(StdMutex::new(options));
    let shared = state.clone();
    let router: crate::test_http::Router = Arc::new(move |method: &str, path: &str, body: &Value| {
        let mut s = shared.lock().unwrap();
        let company = s["sessionCompany"].as_str().unwrap_or("DEFAULT").to_owned();
        match method {
            "TokenResource" if s["badPassword"] == true => (401, json!({"ErrorMessage":"denied"})),
            "TokenResource" => (200, json!({"TokenType":"Bearer","AccessToken":"fictional-token","ExpiresIn":3600})),
            "Login" if s["noSeat"] == true => (400, json!({"ErrorMessage":"Maximum number of users reached","ErrorType":"Ice.Common.BusinessObjectException"})),
            "Login" => (200, json!({"returnObj":"fictional-session"})),
            "Logout" if s["logoutFailure"] == true => (500, json!({"ErrorMessage":"fixture logout failure"})),
            "SetCompany" => {
                let requested = body["newCompany"].as_str().unwrap_or("").to_owned();
                if s["members"].as_array().is_some_and(|m| m.iter().any(|id| id == requested.as_str())) { s["sessionCompany"] = json!(requested); (200, json!({})) }
                else { (400, json!({"ErrorMessage":"Invalid company"})) }
            }
            "GetCurrentValuesFromServer" => (200, json!({"parameters":{"companyID":company,"tenantID":""}})),
            "GetUserFile" => {
                assert!(path.contains(&format!("/odata/{company}/")), "data routes address the session company: {path}");
                let mut row = json!({"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false});
                if !s["compList"].is_null() { row["CompList"] = s["compList"].clone(); }
                let mut reply = json!({"returnObj":{"UserFile":[row]}});
                if !s["userComp"].is_null() { reply["returnObj"]["UserComp"] = s["userComp"].clone(); }
                (200, reply)
            }
            "GetList" if s["listFailure"] == true => (400, json!({"ErrorMessage":"list failed"})),
            "GetList" => (200, json!({"returnObj":{"CompanyList":s["companyList"].clone()}})),
            "GetDatabaseVersion" => (200, json!({"returnObj":"5.2.100"})),
            _ => (200, json!({})),
        }
    });
    (Server::start(None, router), state)
}
fn defaults() -> Value {
    json!({"sessionCompany":"EPIC","members":["EPIC","LAB"],
        "userComp":[{"Company":"LAB","CompanyName":"Test company"},{"Company":"EPIC","CompanyName":"Epic Corp"}],
        "compList":"EPIC~LAB","companyList":[{"Company":"EPIC","Name":"Epic Corp"},{"Company":"LAB","Name":"Test company"},{"Company":"OTHER","Name":"Other"}]})
}
async fn test(server: &Server, company: Option<&str>) -> (Arc<Host>, Reply, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    let mut arguments = json!({"url":server.endpoint,"authMode":"session",
        "credentialSource":{"type":"inline","credentials":{"username":"manager","password":"fictional","apiKey":null}}});
    if let Some(company) = company { arguments["company"] = json!(company); }
    let reply = host.execute(Command { schema_version: 1, method: "connection.test".into(), arguments }).await;
    (host, reply, root)
}

#[tokio::test]
async fn test_connection_lists_accessible_companies_and_logs_out() {
    let (server, _) = server(defaults());
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let result = reply.result.expect("connection.test succeeded");
    assert_eq!(result["companySource"], "userAccess");
    assert_eq!(result["availableCompanies"], json!([{"id":"EPIC","name":"Epic Corp"},{"id":"LAB","name":"Test company"}]), "sorted by id, names from UserComp");
    assert_eq!(result["selectedCompany"], json!({"id":"EPIC","inList":true}));
    assert_eq!(result["transportTrust"]["mode"], "unencrypted", "the loopback fixture is plain http");
    assert_eq!(server.count("Logout"), 1);
    assert_eq!(server.count("GetList"), 0, "UserComp rows need no extra call");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_without_company_uses_discovery_mode_and_never_becomes_active() {
    let (server, _) = server(json!({"sessionCompany":"LAB","members":["EPIC","LAB"],"userComp":[{"Company":"EPIC","CompanyName":"Epic Corp"}]}));
    let (host, reply, _root) = test(&server, None).await;
    let result = reply.result.expect("discovery succeeded");
    assert_eq!(server.count("SetCompany"), 0, "discovery keeps the default session company");
    assert_eq!(result["company"], Value::Null);
    assert_eq!(result["selectedCompany"], Value::Null);
    assert_eq!(result["availableCompanies"], json!([{"id":"EPIC","name":"Epic Corp"}]));
    assert_eq!(server.count("Logout"), 1);
    // Nothing is left connected, and connect still requires a company.
    let disconnect = host.execute(Command { schema_version: 1, method: "connection.disconnect".into(), arguments: json!({}) }).await;
    assert_eq!(disconnect.error.unwrap().code, "CONNECTION_REQUIRED");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_reports_authentication_failure_plainly_and_releases_session() {
    let mut options = defaults(); options["badPassword"] = json!(true);
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let error = reply.error.unwrap();
    assert_eq!(error.code, "ACCESS_DENIED");
    assert_eq!(error.message, "Epicor rejected the sign-in. Check the username, password and API key for this environment.");
    assert_eq!(error.context["connectionStage"], "signIn");
    assert_eq!(server.count("Login"), 0, "no session was opened");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_reports_untrusted_certificate_with_option_off() {
    // The managed-session sequence over TLS with a self-signed test certificate.
    let router: crate::test_http::Router = Arc::new(|_: &str, _: &str, _: &Value| (200, json!({})));
    let server = Server::start(Some("self-signed"), router);
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let error = reply.error.unwrap();
    assert_eq!(error.code, "CERTIFICATE_UNTRUSTED");
    assert!(error.message.contains("turn on 'Allow self-signed certificate' for this profile"));
    assert!(server.calls.lock().unwrap().is_empty());
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_reports_session_or_licence_refusal_plainly() {
    let mut options = defaults(); options["noSeat"] = json!(true);
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let error = reply.error.unwrap();
    assert!(error.message.starts_with("Epicor would not start a session for this account"), "{}", error.message);
    assert_eq!(error.context["connectionStage"], "session");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_flags_saved_company_missing_from_list() {
    let mut options = defaults(); options["members"] = json!(["EPIC","LAB","GONE"]);
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, Some("GONE")).await;
    let result = reply.result.expect("the session accepted the company");
    assert_eq!(result["selectedCompany"], json!({"id":"GONE","inList":false}));
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_falls_back_to_company_list_when_user_access_field_is_absent() {
    let mut options = defaults(); options["userComp"] = Value::Null; options["compList"] = Value::Null;
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let result = reply.result.unwrap();
    assert_eq!(result["companySource"], "companyList");
    assert_eq!(result["availableCompanies"].as_array().unwrap().len(), 3);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn test_connection_logs_out_even_when_company_list_read_fails() {
    let mut options = defaults(); options["userComp"] = Value::Null; options["compList"] = Value::Null; options["listFailure"] = json!(true);
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let result = reply.result.expect("admission still succeeded");
    assert_eq!(result["companySource"], "unavailable");
    assert!(result["companyListDiagnostic"]["code"].is_string());
    assert_eq!(result["admission"], "verifiedSecurityManager");
    assert_eq!(server.count("Logout"), 1);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn company_list_prefers_user_comp_rows_with_names() {
    let (server, _) = server(defaults());
    let (host, reply, _root) = test(&server, Some("EPIC")).await;
    let result = reply.result.unwrap();
    assert!(result["availableCompanies"].as_array().unwrap().iter().all(|row| !row["name"].as_str().unwrap().is_empty()));
    assert_eq!(server.count("GetList"), 0);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn company_list_splits_comp_list_when_user_comp_absent() {
    for (list, expected) in [("EPIC~LAB", 2), ("EPIC,LAB", 2), ("EPIC", 1)] {
        let mut options = defaults(); options["userComp"] = Value::Null; options["compList"] = json!(list);
        let (server, _) = server(options);
        let (host, reply, _root) = test(&server, Some("EPIC")).await;
        let result = reply.result.unwrap();
        assert_eq!(result["companySource"], "userAccess", "{list}");
        let rows = result["availableCompanies"].as_array().unwrap();
        assert_eq!(rows.len(), expected, "{list}");
        assert_eq!(rows[0], json!({"id":"EPIC","name":"Epic Corp"}), "names come from the company list");
        assert!(host.shutdown().await.is_empty());
    }
}

#[tokio::test]
async fn company_list_single_company_matches_lab_shape() {
    // Shape of the redacted Test-2024.2 GetUserFile: one UserComp row and CompList with one ID.
    let options = json!({"sessionCompany":"COMPANY1","members":["COMPANY1"],"compList":"COMPANY1",
        "userComp":[{"Company":"COMPANY1","CompanyName":"Company One"}]});
    let (server, _) = server(options);
    let (host, reply, _root) = test(&server, None).await;
    let result = reply.result.unwrap();
    assert_eq!(result["availableCompanies"], json!([{"id":"COMPANY1","name":"Company One"}]));
    assert!(host.shutdown().await.is_empty());
}

#[test]
fn transport_trust_is_unencrypted_for_http_and_verified_for_https_without_relaxation() {
    let connection = |url: &str| Connection::new(url::Url::parse(url).unwrap(), "EPIC".into(), AuthMode::Session,
        crate::input::Credentials { username: "manager".into(), password: "fictional".into(), api_key: None }, OperatingSettings::default(), Budget::new(1).unwrap()).unwrap();
    assert_eq!(connection("http://erp.example.invalid/K/").transport_trust().mode, crate::transport_trust::TrustMode::Unencrypted);
    // Over https, an unrelaxed working connection was verified by Windows trust;
    // the relaxed mode comes only from the verifier's own observation (transport_trust tests).
    assert_eq!(connection("https://erp.example.invalid/K/").transport_trust().mode, crate::transport_trust::TrustMode::Verified);
    assert_eq!(serde_json::to_value(crate::transport_trust::TrustMode::Unencrypted).unwrap(), "unencrypted");
}

#[tokio::test]
async fn http_connect_reports_unencrypted_trust() {
    // The run journal records this same value at start (application.rs: journal.transport_trust = connection.transport_trust()).
    let (server, _) = server(defaults());
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    let connect = host.execute(Command { schema_version: 1, method: "connection.connect".into(), arguments: json!({"url":server.endpoint,"company":"EPIC","authMode":"session",
        "credentialSource":{"type":"inline","credentials":{"username":"manager","password":"fictional","apiKey":null}}}) }).await;
    assert_eq!(connect.result.expect("connected")["transportTrust"]["mode"], "unencrypted");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn retained_discovery_connection_refuses_start_and_server_change_operations_until_disconnect() {
    let (server, state) = server(defaults());
    state.lock().unwrap()["logoutFailure"] = json!(true);
    let (host, reply, root) = test(&server, None).await;
    let error = reply.error.expect("an uncertain sign-out is reported");
    assert_eq!(error.context["connectionRetained"], true, "{error:?}");
    let before = server.calls.lock().unwrap().len();
    let run = json!({"requestId":uuid::Uuid::new_v4().to_string(),"destination":root.path().join("out"),"outputMode":"separate","categories":[],
        "collectionMethod":"directOnly","temporaryAccess":false,"pluginSettings":{}});
    for (method, arguments) in [("backup.start", run.clone()), ("serverChanges.preview", json!({"fullBackup":true,"run":run})),
        ("companies.list", json!({})), ("run.recover", json!({"id":uuid::Uuid::new_v4().to_string()}))] {
        let reply = host.execute(Command { schema_version: 1, method: method.into(), arguments }).await;
        let error = reply.error.unwrap_or_else(|| panic!("{method} must refuse a company-discovery connection"));
        assert_eq!(error.code, "CONNECTION_REQUIRED", "{method}: {error:?}");
        assert!(error.message.contains("Disconnect to retry the sign-out"), "{method}: {}", error.message);
        assert_eq!(error.context["recoveryOperation"], "connection.disconnect");
    }
    assert_eq!(server.calls.lock().unwrap().len(), before, "no request reached the server");
    // Disconnect is still available and retries the sign-out.
    state.lock().unwrap()["logoutFailure"] = json!(false);
    let disconnect = host.execute(Command { schema_version: 1, method: "connection.disconnect".into(), arguments: json!({}) }).await;
    assert!(disconnect.error.is_none(), "{:?}", disconnect.error);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn retained_test_connection_with_company_refuses_work_until_disconnect() {
    let (server, state) = server(defaults());
    state.lock().unwrap()["logoutFailure"] = json!(true);
    let (host, reply, root) = test(&server, Some("EPIC")).await;
    let error = reply.error.expect("an uncertain sign-out is reported");
    assert_eq!(error.context["connectionRetained"], true, "{error:?}");
    // A test is not a connect: no authorization-session rotation and no preview clear.
    let session = host.authorization_session.read().await.clone();
    assert!(host.server_change_previews.lock().await.is_empty());
    let before = server.calls.lock().unwrap().len();
    let run = json!({"requestId":uuid::Uuid::new_v4().to_string(),"destination":root.path().join("out"),"outputMode":"separate","categories":[],
        "collectionMethod":"directOnly","temporaryAccess":false,"pluginSettings":{}});
    for (method, arguments) in [("backup.start", run.clone()), ("serverChanges.preview", json!({"fullBackup":true,"run":run})),
        ("companies.list", json!({})), ("run.recover", json!({"id":uuid::Uuid::new_v4().to_string()}))] {
        let reply = host.execute(Command { schema_version: 1, method: method.into(), arguments }).await;
        let error = reply.error.unwrap_or_else(|| panic!("{method} must refuse a retained test connection"));
        assert_eq!(error.code, "CONNECTION_REQUIRED", "{method}: {error:?}");
        assert!(error.message.contains("Disconnect to retry the sign-out"), "{method}: {}", error.message);
        assert_eq!(error.context["recoveryOperation"], "connection.disconnect");
    }
    assert_eq!(server.calls.lock().unwrap().len(), before, "no request reached the server");
    assert_eq!(*host.authorization_session.read().await, session);
    state.lock().unwrap()["logoutFailure"] = json!(false);
    let disconnect = host.execute(Command { schema_version: 1, method: "connection.disconnect".into(), arguments: json!({}) }).await;
    assert!(disconnect.error.is_none(), "{:?}", disconnect.error);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn empty_company_refused_by_mcp_start_with_code_and_message() {
    let (server, _) = server(defaults());
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    for company in [None, Some("")] {
        let mut arguments = json!({"url":server.endpoint,"authMode":"session",
            "credentialSource":{"type":"inline","credentials":{"username":"manager","password":"fictional","apiKey":null}}});
        if let Some(company) = company { arguments["company"] = json!(company); }
        let reply = host.execute(Command { schema_version: 1, method: "connection.connect".into(), arguments }).await;
        let error = reply.error.unwrap();
        assert_eq!(error.code, "COMPANY_REQUIRED");
        assert!(error.message.contains("run connection.test without a company"), "{}", error.message);
    }
    assert_eq!(server.count("Login"), 0);
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn profile_option_does_not_follow_overridden_endpoint() {
    let router = || -> crate::test_http::Router { Arc::new(|method: &str, _: &str, _: &Value| match method {
        "GetUserFile" => (200, json!({"returnObj":{"UserFile":[{"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}})),
        "GetDatabaseVersion" => (200, json!({"returnObj":"5.2.100"})),
        _ => (200, json!({})),
    }) };
    let saved = Server::start(Some("self-signed"), router());
    let other = Server::start(Some("self-signed"), router());
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    let upsert = host.execute(Command { schema_version: 1, method: "profiles.upsert".into(),
        arguments: json!({"id":"test","url":saved.endpoint,"authMode":"basicApiKey","allowUntrustedCertificate":true,"defaults":{"company":"EPIC"}}) }).await;
    assert!(upsert.ok, "{:?}", upsert.error);
    let test = |url: Option<&url::Url>| {
        let mut arguments = json!({"profileId":"test","credentialSource":{"type":"inline","credentials":{"username":"manager","password":"fixture","apiKey":"fixture"}}});
        if let Some(url) = url { arguments["url"] = json!(url); }
        Command { schema_version: 1, method: "connection.test".into(), arguments }
    };
    let same = host.execute(test(None)).await;
    assert!(same.ok, "the profile's own endpoint uses its option: {:?}", same.error);
    let overridden = host.execute(test(Some(&other.endpoint))).await;
    assert_eq!(overridden.error.unwrap().code, "CERTIFICATE_UNTRUSTED", "the option does not follow an overridden endpoint");
    assert!(other.calls.lock().unwrap().is_empty());
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn profile_upsert_without_company_saves_with_warning() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    for defaults in [json!({}), json!({"company":""})] {
        let reply = host.execute(Command { schema_version: 1, method: "profiles.upsert".into(),
            arguments: json!({"id":"test","url":"https://erp.example.invalid/Kinetic","defaults":defaults}) }).await;
        let result = reply.result.unwrap();
        assert_eq!(result["saved"], true);
        assert_eq!(result["warnings"][0]["code"], "COMPANY_NOT_SET");
    }
    let listed = host.execute(Command { schema_version: 1, method: "profiles.list".into(), arguments: json!({}) }).await.result.unwrap();
    assert_eq!(listed[0]["defaults"]["company"], Value::Null, "an empty company is stored as not chosen");
    let reply = host.execute(Command { schema_version: 1, method: "profiles.upsert".into(),
        arguments: json!({"id":"test","url":"https://erp.example.invalid/Kinetic","defaults":{"company":"EPIC"}}) }).await;
    assert!(reply.result.unwrap().get("warnings").is_none());
    assert!(host.shutdown().await.is_empty());
}
