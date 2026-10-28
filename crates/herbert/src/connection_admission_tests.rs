//! Real HTTP admission checks. No protected route may run without a verified role.
use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct Fixture {
    endpoint: Url,
    user: Arc<StdMutex<Value>>,
    calls: Arc<StdMutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(user: Value) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = Url::parse(&format!(
            "http://{}/fixture/",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let user = Arc::new(StdMutex::new(user));
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let u = user.clone();
        let c = calls.clone();
        let done = stop.clone();
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if done.load(Ordering::SeqCst) {
                    break;
                }
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let method = first
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned();
                let mut has_session = false;
                let mut has_key = false;
                let mut bearer = false;
                let mut length = 0;
                let mut has_length = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("sessioninfo") {
                            has_session = v.contains("fictional-session");
                        }
                        if k.eq_ignore_ascii_case("x-api-key") {
                            has_key = true;
                        }
                        if k.eq_ignore_ascii_case("authorization") {
                            bearer = v.trim().starts_with("Bearer ");
                        }
                        if k.eq_ignore_ascii_case("content-length") {
                            has_length = true;
                            length = v.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                c.lock().unwrap().push(method.clone());
                // SaaS rejects a token-only service read. SessionInfo permits the keyless session path.
                let refused = matches!(method.as_str(), "GetUserFile" | "GetDatabaseVersion")
                    && bearer
                    && !has_key
                    && !has_session;
                let value = match method.as_str() {
                    "TokenResource" => {
                        if u.lock().unwrap()["tokenFailure"] == json!(true) {
                            json!({})
                        } else {
                            json!({"TokenType":"Bearer","AccessToken":"fictional-token","ExpiresIn":3600})
                        }
                    }
                    "GetUserFile" => u.lock().unwrap().clone(),
                    "Login" => json!({"returnObj":"fictional-session"}),
                    "GetCurrentValuesFromServer" => json!({"parameters":{"companyID":"FIXTURE"}}),
                    "GetDatabaseVersion" => json!({"returnObj":"5.2.100"}),
                    "Lease" => json!({"oJson":json!({"accepted":true}).to_string()}),
                    _ => json!({}),
                };
                // Model strict HTTP frontends: an empty token POST still needs explicit length.
                let missing_token_length = method == "TokenResource" && !has_length;
                let flags = u.lock().unwrap().clone();
                let forced_admission_failure =
                    method == "GetUserFile" && flags["admissionFailure"] == json!(true);
                let forced_logout_failure =
                    method == "Logout" && flags["logoutFailure"] == json!(true);
                let status = if missing_token_length { "411 Length Required" }
                    else if forced_logout_failure { "503 Service Unavailable" }
                    else if refused || forced_admission_failure { "403 Forbidden" }
                    else { "200 OK" };
                let text = serde_json::to_string(&value).unwrap();
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
            }
        });
        Self {
            endpoint,
            user,
            calls,
            stop,
            worker: Some(worker),
        }
    }
    fn connection(&self, mode: AuthMode) -> Arc<Connection> {
        Connection::new(
            self.endpoint.clone(),
            "FIXTURE".into(),
            mode,
            Credentials {
                username: "tester".into(),
                password: "fictional".into(),
                api_key: if mode == AuthMode::BasicApiKey {
                    Some("fictional-key".into())
                } else {
                    None
                },
            },
            OperatingSettings::default(),
            Budget::new(4).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.socket_addrs(|| None).unwrap()[0]);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn user() -> Value {
    json!({"returnObj":{"UserFile":[{"DcdUserID":"tester","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}})
}

async fn assert_protected_routes_refused(c: &Connection) {
    let cancel = CancellationToken::new();
    assert!(
        c.native_read("Ice.BO.FixtureSvc", "GetRows", json!({}), false, &cancel)
            .await
            .is_err()
    );
    assert!(
        c.native_entity_page(
            "Ice.BO.FixtureSvc",
            "Rows",
            &Default::default(),
            1,
            1,
            &cancel
        )
        .await
        .is_err()
    );
    assert!(
        c.artifact_call("Ice.BO.FixtureSvc", "Update", json!({}), &cancel)
            .await
            .is_err()
    );
    assert!(
        c.artifact_function_call("Fixture", "Run", json!({}), &cancel)
            .await
            .is_err()
    );
    assert!(
        c.owned_write(
            "Ice.Lib.EfxLibraryDesignerSvc",
            "ImportLibrary",
            json!({}),
            &cancel
        )
        .await
        .is_err()
    );
    assert!(
        c.security_read("Ice.BO.SecGroupSvc", "GetByID", json!({}), &cancel)
            .await
            .is_err()
    );
    assert!(
        c.helper_metadata("GetDefaults", json!({}), &cancel)
            .await
            .is_err()
    );
    assert!(c.caller_access(&cancel).await.is_err());
}

#[tokio::test]
async fn unconnected_account_cannot_dispatch_any_protected_route() {
    for mode in [AuthMode::BasicApiKey, AuthMode::Session] {
        let server = Fixture::new(user());
        let c = server.connection(mode);
        assert_protected_routes_refused(&c).await;
        assert!(server.calls.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn invalid_role_or_identity_stops_before_collection() {
    let mut cases = vec![
        json!({}),
        json!({"returnObj":{"UserFile":[]}}),
        json!({"returnObj":{"UserFile":[{},{}]}}),
    ];
    for (field, value) in [
        ("SecurityMgr", json!(false)),
        ("SecurityMgr", Value::Null),
        ("SecurityMgr", json!("true")),
        ("UserDisabled", json!(true)),
        ("UserDisabled", Value::Null),
        ("LockedOut", json!(true)),
        ("LockedOut", Value::Null),
        ("DcdUserID", json!("other")),
    ] {
        let mut row = user();
        row["returnObj"]["UserFile"][0][field] = value;
        cases.push(row);
    }
    for mode in [AuthMode::BasicApiKey, AuthMode::Session] {
        for row in &cases {
            let server = Fixture::new(row.clone());
            let c = server.connection(mode);
            assert!(c.connect(&CancellationToken::new()).await.is_err());
            assert!(c.caller().await.is_none());
            let before = server.calls.lock().unwrap().clone();
            assert!(before.iter().all(|m| matches!(
                m.as_str(),
                "TokenResource"
                    | "Login"
                    | "SetCompany"
                    | "GetCurrentValuesFromServer"
                    | "GetUserFile"
            )));
            assert_protected_routes_refused(&c).await;
            assert_eq!(*server.calls.lock().unwrap(), before);
        }
    }
}

#[tokio::test]
async fn failed_reconnect_revokes_previous_admission() {
    for mode in [AuthMode::BasicApiKey, AuthMode::Session] {
        let server = Fixture::new(user());
        let c = server.connection(mode);
        c.connect(&CancellationToken::new()).await.unwrap();
        server.user.lock().unwrap()["returnObj"]["UserFile"][0]["SecurityMgr"] = json!(false);
        assert!(c.connect(&CancellationToken::new()).await.is_err());
        assert!(c.caller().await.is_none());
        let before = server.calls.lock().unwrap().clone();
        assert_protected_routes_refused(&c).await;
        assert_eq!(*server.calls.lock().unwrap(), before);
    }
}

#[tokio::test]
async fn failed_token_renewal_cannot_reuse_old_admission() {
    let server = Fixture::new(user());
    let c = server.connection(AuthMode::Session);
    c.connect(&CancellationToken::new()).await.unwrap();
    c.session.lock().await.as_mut().unwrap().expires = Instant::now();
    server.user.lock().unwrap()["tokenFailure"] = json!(true);
    assert!(
        c.native_read(
            "Ice.BO.FixtureSvc",
            "GetRows",
            json!({}),
            false,
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(c.caller().await.is_none());
    let before = server.calls.lock().unwrap().clone();
    assert!(!before.iter().any(|m| m == "GetRows"));
    assert_protected_routes_refused(&c).await;
    assert_eq!(*server.calls.lock().unwrap(), before);
}

#[tokio::test]
async fn role_is_checked_once_per_connection_not_on_reads_or_renewal() {
    for mode in [AuthMode::BasicApiKey, AuthMode::Session] {
        let server = Fixture::new(user());
        let c = server.connection(mode);
        let cancel = CancellationToken::new();
        c.connect(&cancel).await.unwrap();
        assert_eq!(
            server
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == "GetUserFile")
                .count(),
            1
        );
        // Later role enforcement belongs to Epicor, not repeated host lookups.
        server.user.lock().unwrap()["returnObj"]["UserFile"][0]["SecurityMgr"] = json!(false);
        if mode == AuthMode::Session {
            c.session.lock().await.as_mut().unwrap().expires = Instant::now();
        }
        c.native_read("Ice.BO.FixtureSvc", "GetRows", json!({}), false, &cancel)
            .await
            .unwrap();
        c.refresh_access(&cancel).await.unwrap();
        assert_eq!(
            server
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == "GetUserFile")
                .count(),
            1
        );
        assert!(c.connect(&cancel).await.is_err());
        assert_eq!(
            server
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.as_str() == "GetUserFile")
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn managed_session_without_api_key_is_established_before_role_check() {
    let server = Fixture::new(user());
    let c = server.connection(AuthMode::Session);
    let cancel = CancellationToken::new();
    c.connect(&cancel).await.unwrap();
    assert_eq!(
        *server.calls.lock().unwrap(),
        vec![
            "TokenResource",
            "Login",
            "SetCompany",
            "GetCurrentValuesFromServer",
            "GetUserFile",
            "GetDatabaseVersion"
        ]
    );
    c.logout(&cancel).await.unwrap();
    assert_eq!(server.calls.lock().unwrap().last().unwrap(), "Logout");
}

#[tokio::test]
async fn failed_host_admission_retains_only_uncertain_sessions_for_logout_retry() {
    use crate::application::{Command, Host};
    let root = tempfile::tempdir().unwrap();
    let server = Fixture::new(json!({
        "admissionFailure": true,
        "logoutFailure": true,
        "returnObj": {"UserFile": [{
            "DcdUserID": "tester",
            "SecurityMgr": true,
            "UserDisabled": false,
            "LockedOut": false
        }]}
    }));
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    let input = json!({
        "url": server.endpoint,
        "company": "FIXTURE",
        "authMode": "session",
        "credentialSource": {
            "type": "inline",
            "credentials": {"username":"tester","password":"fictional","apiKey":null}
        }
    });
    for method in ["connection.test", "connection.connect"] {
        let before = server.calls.lock().unwrap().len();
        let reply = host.execute(Command {
            schema_version: 1,
            method: method.into(),
            arguments: input.clone(),
        }).await;
        let error = reply.error.unwrap();
        assert_eq!(error.code, "ACCESS_DENIED");
        assert_eq!(error.context["cleanup"]["code"], "LOGOUT_UNCERTAIN");
        assert_eq!(error.context["connectionRetained"], true);
        assert_eq!(error.context["recoveryOperation"], "connection.disconnect");
        let calls = server.calls.lock().unwrap()[before..].to_vec();
        assert_eq!(calls.iter().filter(|call| *call == "Login").count(), 1);
        assert_eq!(calls.iter().filter(|call| *call == "Logout").count(), 1);

        server.user.lock().unwrap()["logoutFailure"] = json!(false);
        let disconnected = host.execute(Command {
            schema_version: 1,
            method: "connection.disconnect".into(),
            arguments: json!({}),
        }).await;
        assert!(disconnected.ok, "{:?}", disconnected.error);
        assert_eq!(server.calls.lock().unwrap()[before..].iter().filter(|call| *call == "Login").count(), 1);
        server.user.lock().unwrap()["logoutFailure"] = json!(true);
    }

    server.user.lock().unwrap()["logoutFailure"] = json!(false);
    let reply = host.execute(Command {
        schema_version: 1,
        method: "connection.test".into(),
        arguments: input,
    }).await;
    assert_eq!(reply.error.unwrap().code, "ACCESS_DENIED");
    let absent = host.execute(Command {
        schema_version: 1,
        method: "connection.disconnect".into(),
        arguments: json!({}),
    }).await;
    assert_eq!(absent.error.unwrap().code, "CONNECTION_REQUIRED");
    assert!(host.shutdown().await.is_empty());
}

#[tokio::test]
async fn lease_renews_session_with_all_download_slots_and_read_guards_held() {
    let server = Fixture::new(user());
    let connection = server.connection(AuthMode::Session);
    let cancel = CancellationToken::new();
    connection.connect(&cancel).await.unwrap();
    let mut downloads = Vec::new();
    for _ in 0..connection.budget.ceiling() {
        downloads.push(connection.budget.acquire(&cancel).await.unwrap());
    }
    let inflight = connection.lifecycle.read().await;
    connection.session.lock().await.as_mut().unwrap().expires = Instant::now();
    let renewed = tokio::time::timeout(
        Duration::from_secs(2),
        connection.lease_call(
            "Fixture",
            json!({"action":"renew","operationId":"fixture"}),
            &cancel,
        ),
    )
    .await
    .expect("lease control waited for collection capacity")
    .unwrap();
    assert_eq!(renewed["accepted"], true);
    assert_eq!(
        server
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "TokenResource")
            .count(),
        2
    );
    drop(inflight);
    drop(downloads);
    connection.logout(&cancel).await.unwrap();
}
