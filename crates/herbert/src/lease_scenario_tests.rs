//! Lease scenarios against an in-process fake lease registry with a simulated clock.
//! Heartbeat timing runs under paused tokio time, so the 30 s renewal and 5 min expiry
//! advance instantly. Claim, reap, release and competing runs go through the real host
//! code and HTTP routes against a loopback server backed by the same registry.
use super::*;
use std::collections::BTreeMap as Map;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;

const COMPANY: &str = "C";
const CALLER: &str = "manager";

/// The server side of the lease protocol, as Herbert's lease host implements it:
/// operations hold resources until released or expired (5 min after their last
/// accepted acquire or renewal); only inactive resources can be claimed and reaped.
struct Registry {
    /// Paused-tokio tests follow the tokio clock; HTTP tests advance `offset` by hand.
    epoch: Option<tokio::time::Instant>,
    offset: Duration,
    expiry: Duration,
    operations: Map<String, (Duration, bool)>,
    resources: Map<String, Value>,
    libraries: Map<String, Value>,
    /// ExportLibrary packages by library ID.
    packages: Map<String, Value>,
    silent: bool,
    refuse_renewal: bool,
    renewals: usize,
    deletes: Vec<String>,
}
impl Registry {
    fn new(epoch: Option<tokio::time::Instant>) -> Self {
        Self { epoch, offset: Duration::ZERO, expiry: EXPIRY, operations: Map::new(), resources: Map::new(), libraries: Map::new(), packages: Map::new(),
            silent: false, refuse_renewal: false, renewals: 0, deletes: vec![] }
    }
    fn now(&self) -> Duration {
        self.epoch.map_or(Duration::ZERO, |epoch| epoch.elapsed()) + self.offset
    }
    fn live(&self, operation: &str) -> bool {
        self.operations.get(operation).is_some_and(|(expires, released)| !released && self.now() < *expires)
    }
    fn active(&self, resource: &Value) -> bool {
        resource["operation"].as_str().is_some_and(|operation| self.live(operation))
    }
    fn handle(&mut self, request: &Value) -> Result<Value, Diagnostic> {
        if self.silent {
            return Err(Diagnostic::new("TRANSPORT_FAILED", "fixture: no response"));
        }
        let operation = request["operationId"].as_str().unwrap_or_default().to_owned();
        let now = self.now();
        Ok(match request["action"].as_str().unwrap_or_default() {
            "acquire" => {
                let wanted = request["resources"].as_array().cloned().unwrap_or_default();
                let held_elsewhere = wanted.iter().any(|item| self.resources.get(item["globalId"].as_str().unwrap_or_default())
                    .is_some_and(|held| self.active(held) && held["operation"] != operation.as_str()));
                if held_elsewhere {
                    json!({"accepted":false})
                } else {
                    self.operations.insert(operation.clone(), (now + self.expiry, false));
                    for item in wanted {
                        let mut held = item.clone();
                        held["operation"] = json!(operation);
                        held["runs"] = json!([operation]);
                        held["state"] = json!("leased");
                        self.resources.insert(item["globalId"].as_str().unwrap().to_owned(), held);
                    }
                    json!({"accepted":true})
                }
            }
            "renew" => {
                self.renewals += 1;
                if !self.refuse_renewal && self.live(&operation) {
                    self.operations.insert(operation, (now + self.expiry, false));
                    json!({"accepted":true})
                } else {
                    json!({"accepted":false})
                }
            }
            "release" => {
                if self.live(&operation) {
                    self.operations.insert(operation, (now, true));
                    json!({"accepted":true})
                } else {
                    json!({"accepted":false})
                }
            }
            "observe" => {
                for item in request["resources"].as_array().cloned().unwrap_or_default() {
                    self.resources.entry(item["globalId"].as_str().unwrap().to_owned()).or_insert(item);
                }
                json!({"accepted":true})
            }
            "claim" => {
                let target = request["globalId"].as_str().unwrap_or_default().to_owned();
                let claimable = self.resources.get(&target).is_some_and(|r| !self.active(r) && r["state"] != "claimed");
                if claimable {
                    let resource = self.resources.get_mut(&target).unwrap();
                    resource["state"] = json!("claimed");
                    resource["claimId"] = json!(uuid::Uuid::new_v4().to_string());
                    json!({"resources":[resource.clone()]})
                } else {
                    json!({"resources":[]})
                }
            }
            "inspect" => {
                let rows = self.resources.values().map(|r| { let mut row = r.clone(); row["active"] = json!(self.active(r)); row }).collect::<Vec<_>>();
                json!({"resources":rows})
            }
            "reap" | "complete" => {
                let target = request["globalId"].as_str().unwrap_or_default();
                let claimed = self.resources.get(target).is_some_and(|r| r["claimId"] == request["claimId"]);
                if claimed && request["action"] == "complete" {
                    self.resources.remove(target);
                }
                json!({"accepted":claimed})
            }
            other => panic!("unexpected lease action {other}"),
        })
    }
    /// A Herbert helper library as the native designer stores it.
    fn install(&mut self, role: &str) -> crate::native_artifact_store::NativeIdentity {
        let generation = uuid::Uuid::new_v4();
        let id = format!("H{}", &generation.simple().to_string()[..29]);
        let notes = if role == "host" { format!("Herbert lease host v1\nHerbert artifact {generation}") } else { format!("Herbert artifact {generation}") };
        let row = json!({"LibraryID":id,"GlobalID":generation.to_string(),"Notes":notes,"OwnedByCompany":COMPANY,"SysRowID":uuid::Uuid::new_v4().to_string(),"Owner":CALLER});
        self.libraries.insert(id.clone(), row.clone());
        crate::native_artifact_store::NativeIdentity { id, company: COMPANY.into(), row_id: row["SysRowID"].as_str().unwrap().into(),
            global_id: Some(generation.to_string()), owner: Some(CALLER.into()), updatable: None, marker: notes }
    }
}

/// Loopback Epicor: admission, EfxLibraryDesigner GetLibrary/ApplyChanges and the lease host's
/// Lease function, all answered from one shared registry.
struct Server { endpoint: url::Url, registry: Arc<StdMutex<Registry>>, stop: Arc<AtomicBool>, worker: Option<std::thread::JoinHandle<()>> }
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = url::Url::parse(&format!("http://{}/fixture/", listener.local_addr().unwrap())).unwrap();
        let registry = Arc::new(StdMutex::new(Registry::new(None)));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, done) = (registry.clone(), stop.clone());
        let worker = std::thread::spawn(move || for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            if done.load(Ordering::SeqCst) { break; }
            let mut reader = BufReader::new(&mut stream);
            let mut first = String::new();
            if reader.read_line(&mut first).is_err() { continue; }
            let method = first.split_whitespace().nth(1).unwrap_or("").rsplit('/').next().unwrap_or("").to_owned();
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() { break; }
                if let Some((key, value)) = line.split_once(':') { if key.eq_ignore_ascii_case("content-length") { length = value.trim().parse().unwrap_or(0); } }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            drop(reader);
            let args: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let mut registry = shared.lock().unwrap();
            let value = match method.as_str() {
                "GetUserFile" => json!({"returnObj":{"UserFile":[{"DcdUserID":CALLER,"SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}}),
                "GetDatabaseVersion" => json!({"returnObj":"12.2.100.9"}),
                "GetLibrary" => {
                    let row = registry.libraries.get(args["libraryId"].as_str().unwrap_or_default()).cloned();
                    json!({"returnObj":{"EfxLibrary":row.into_iter().collect::<Vec<_>>(),"EfxFunction":[]}})
                }
                "ExportLibrary" => {
                    use base64::Engine;
                    match registry.packages.get(args["libraryID"].as_str().unwrap_or_default()) {
                        Some(package) => json!({"returnObj":base64::engine::general_purpose::STANDARD.encode([&[0xef, 0xbb, 0xbf][..], package.to_string().as_bytes()].concat())}),
                        None => json!({"returnObj":null}),
                    }
                }
                "ApplyChangesWithDiagnostics" => {
                    for row in args["libraryTableset"]["EfxLibrary"].as_array().cloned().unwrap_or_default() {
                        if row["RowMod"] == "D" {
                            let id = row["LibraryID"].as_str().unwrap_or_default().to_owned();
                            registry.libraries.remove(&id);
                            registry.deletes.push(id);
                        }
                    }
                    json!({"returnObj":{},"diagnostics":[]})
                }
                "Lease" => {
                    let request: Value = serde_json::from_str(args["iRequest"].as_str().unwrap_or("null")).unwrap_or(Value::Null);
                    match registry.handle(&request) {
                        Ok(reply) => json!({"oJson":reply.to_string()}),
                        Err(_) => { drop(registry); continue; } // no response: the connection just closes
                    }
                }
                other => json!({"unexpected":other}),
            };
            drop(registry);
            let text = value.to_string();
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
        });
        Self { endpoint, registry, stop, worker: Some(worker) }
    }
    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> { self.registry.lock().unwrap() }
    fn advance(&self, by: Duration) { self.registry().offset += by; }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.socket_addrs(|| None).unwrap()[0]);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

/// A released Herbert helper's package as ExportLibrary returns it for `id`; `tamper` changes one function's code.
fn helper_package(id: &str, tamper: bool) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../helpers/host/HerbertHost-5.2.100.efxj");
    let bytes = std::fs::read(path).unwrap();
    let mut package: Value = serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes)).unwrap();
    package["LibraryId"] = json!(id);
    package["Mode"] = json!("Backup");
    if tamper {
        package["Library"]["Functions"][0]["Body"] = json!("{\"Code\":\"return;\"}");
    }
    package
}
fn leftover(identity: &crate::native_artifact_store::NativeIdentity) -> crate::storage::ForeignPendingHelper {
    crate::storage::ForeignPendingHelper { library_id: identity.id.clone(), global_id: identity.global_id.clone().unwrap(), role: "plugin".into(), state: "foreign".into() }
}

struct Case { _root: tempfile::TempDir, host: Arc<Host>, run: Arc<Run> }
fn case() -> Case {
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().join("state"), root.path().join("plugins"));
    let mut journal = storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company = COMPANY.into();
    journal.caller = CALLER.into();
    let store = Storage::open(root.path().join("output").as_path(), false, journal).unwrap();
    let run = Arc::new(Run { storage: Arc::new(std::sync::Mutex::new(store)), cancel: CancellationToken::new(), paused: AtomicBool::new(false),
        wake: Notify::new(), active: AtomicBool::new(true), network: true, helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()), shared_reads: Default::default(), helper_instances: Mutex::new(BTreeMap::new()),
        collection_context: Mutex::new(None) });
    Case { _root: root, host, run }
}
fn run_id(case: &Case) -> String { case.run.storage.lock().unwrap().journal.run_id.clone() }
async fn connection(server: &Server) -> Arc<Connection> {
    let connection = Connection::new(server.endpoint.clone(), COMPANY.into(), AuthMode::BasicApiKey,
        crate::input::Credentials { username: CALLER.into(), password: "fixture".into(), api_key: Some("fixture".into()) },
        OperatingSettings::default(), Budget::new(4).unwrap()).unwrap();
    connection.connect(&CancellationToken::new()).await.unwrap();
    connection
}
fn lost_flag(case: &Case, host: &Arc<Host>) -> Arc<AtomicBool> {
    let id = run_id(case);
    let keepers = host.server_leases.try_lock().unwrap();
    keepers.get(&id).expect("this run holds a lease").lost.clone()
}

/// Heartbeat under paused time: `renew` talks to the registry directly with the same requests the
/// host sends. Returns the run's lost flag and the task.
fn heartbeat(case: &Case, registry: Arc<StdMutex<Registry>>, operation: &str, stop: CancellationToken) -> (Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let lost = Arc::new(AtomicBool::new(false));
    let (run, flag, operation) = (case.run.clone(), lost.clone(), operation.to_owned());
    let acquired = Instant::now();
    let task = tokio::spawn(async move {
        let renew = |_token: CancellationToken| {
            let (registry, operation) = (registry.clone(), operation.clone());
            async move { registry.lock().unwrap().handle(&json!({"action":"renew","operationId":operation})) }
        };
        keep_renewing(renew, &run, &stop, &*flag, HEARTBEAT, EXPIRY, MARGIN, acquired).await;
    });
    (lost, task)
}
fn acquired_registry(operation: &str) -> Arc<StdMutex<Registry>> {
    let mut registry = Registry::new(Some(tokio::time::Instant::now()));
    assert_eq!(registry.handle(&json!({"action":"acquire","operationId":operation,"resources":[{"globalId":"g-1","libraryId":"H1","role":"host"}]})).unwrap()["accepted"], true);
    Arc::new(StdMutex::new(registry))
}

// 1
#[tokio::test(start_paused = true)]
async fn renewal_every_30_seconds_keeps_the_lease_across_many_windows() {
    let case = case();
    let registry = acquired_registry("run-a");
    let stop = CancellationToken::new();
    let (lost, task) = heartbeat(&case, registry.clone(), "run-a", stop.clone());
    tokio::time::sleep(Duration::from_secs(20 * 60)).await; // four lease windows
    assert!(!lost.load(Ordering::SeqCst));
    assert!(registry.lock().unwrap().live("run-a"), "the server still holds it");
    assert!((38..=41).contains(&registry.lock().unwrap().renewals), "one renewal per 30 s: {}", registry.lock().unwrap().renewals);
    stop.cancel();
    task.await.unwrap();
}

// 2
#[tokio::test(start_paused = true)]
async fn silent_server_marks_the_lease_lost_15_seconds_before_the_server_expires_it() {
    let case = case();
    let registry = acquired_registry("run-a");
    registry.lock().unwrap().silent = true;
    let started = Instant::now();
    let (lost, task) = heartbeat(&case, registry.clone(), "run-a", CancellationToken::new());
    task.await.unwrap();
    assert!(lost.load(Ordering::SeqCst));
    let at = started.elapsed();
    assert_eq!(at.as_secs(), (EXPIRY - MARGIN).as_secs(), "lost at expiry minus margin: {at:?}");
    let mut registry = registry.lock().unwrap();
    registry.silent = false;
    assert!(registry.live("run-a"), "the host stopped believing it held the lease while the server still held it");
    assert_eq!(case.run.storage.lock().unwrap().journal.diagnostics.iter().filter(|d| d.code == "SERVER_LEASE_LOST").count(), 1);
}

// 3
#[tokio::test(start_paused = true)]
async fn transient_renewal_failure_is_retried_and_the_lease_survives() {
    let case = case();
    let registry = acquired_registry("run-a");
    let stop = CancellationToken::new();
    let (lost, task) = heartbeat(&case, registry.clone(), "run-a", stop.clone());
    tokio::time::sleep(Duration::from_secs(100)).await;
    registry.lock().unwrap().silent = true; // three missed heartbeats
    tokio::time::sleep(Duration::from_secs(95)).await;
    registry.lock().unwrap().silent = false;
    tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    assert!(!lost.load(Ordering::SeqCst), "failures inside the window are retried, not loss");
    assert!(registry.lock().unwrap().live("run-a"));
    stop.cancel();
    task.await.unwrap();
}

// 4
#[tokio::test(start_paused = true)]
async fn refused_renewal_marks_the_lease_lost_at_once() {
    let case = case();
    let registry = acquired_registry("run-a");
    registry.lock().unwrap().refuse_renewal = true;
    let started = Instant::now();
    let (lost, task) = heartbeat(&case, registry, "run-a", CancellationToken::new());
    task.await.unwrap();
    assert!(lost.load(Ordering::SeqCst));
    assert_eq!(started.elapsed().as_secs(), HEARTBEAT.as_secs(), "lost at the first refused renewal, not at expiry");
}

// 5
#[tokio::test(start_paused = true)]
async fn crash_mid_run_leaves_a_lease_that_expires_and_becomes_claimable() {
    let case = case();
    let registry = acquired_registry("run-a");
    let stop = CancellationToken::new();
    let (_lost, task) = heartbeat(&case, registry.clone(), "run-a", stop.clone());
    tokio::time::sleep(Duration::from_secs(90)).await;
    stop.cancel(); // the run's process is gone: no more renewals and no release
    task.await.unwrap();
    let claim = json!({"action":"claim","globalId":"g-1"});
    assert!(registry.lock().unwrap().handle(&claim).unwrap()["resources"].as_array().unwrap().is_empty(), "still leased right after the crash");
    tokio::time::sleep(EXPIRY).await;
    let claimed = registry.lock().unwrap().handle(&claim).unwrap();
    assert_eq!(claimed["resources"][0]["globalId"], "g-1", "claimable once the last renewal's window has passed");
}

// 6
#[tokio::test]
async fn acquiring_registers_the_host_and_each_helper_under_this_runs_operation() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    let helper = server.registry().install("plugin");
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    case.host.keep_helper_alive(&case.run, &connection, &helper, false, &CancellationToken::new()).await.unwrap();
    let registry = server.registry();
    let operation = run_id(&case);
    assert!(registry.live(&operation));
    for identity in [&host_identity, &helper] {
        assert_eq!(registry.resources[identity.global_id.as_ref().unwrap()]["operation"], operation.as_str());
    }
    drop(registry);
    assert!(case.host.lease_gate(&case.run).await.unwrap().is_some(), "new helper work is admitted under a held lease");
}

// 7
#[tokio::test]
async fn expired_lease_is_taken_over_and_its_helper_reaped() {
    let server = Server::new();
    let connection = connection(&server).await;
    let (first, second) = (case(), case());
    let host_a = server.registry().install("host");
    let helper = server.registry().install("plugin");
    first.host.keep_helper_alive(&first.run, &connection, &host_a, true, &CancellationToken::new()).await.unwrap();
    first.host.keep_helper_alive(&first.run, &connection, &helper, false, &CancellationToken::new()).await.unwrap();
    // The first run crashes: its keeper is gone without a release.
    first.host.server_leases.lock().await.clear();
    server.advance(EXPIRY + Duration::from_secs(1));
    let host_b = server.registry().install("host");
    second.host.keep_helper_alive(&second.run, &connection, &host_b, true, &CancellationToken::new()).await.unwrap();
    let removed = second.host.cleanup_leased_helper(&connection, &host_b.id, &helper.id, helper.global_id.as_ref().unwrap()).await.unwrap();
    assert!(removed);
    let registry = server.registry();
    assert_eq!(registry.deletes, vec![helper.id.clone()]);
    assert!(!registry.libraries.contains_key(&helper.id));
    assert!(!registry.resources.contains_key(helper.global_id.as_ref().unwrap()), "the reap was completed");
}

// 8
#[tokio::test]
async fn live_lease_is_never_reaped() {
    let server = Server::new();
    let connection = connection(&server).await;
    let (owner, other) = (case(), case());
    let host_a = server.registry().install("host");
    let helper = server.registry().install("plugin");
    owner.host.keep_helper_alive(&owner.run, &connection, &host_a, true, &CancellationToken::new()).await.unwrap();
    owner.host.keep_helper_alive(&owner.run, &connection, &helper, false, &CancellationToken::new()).await.unwrap();
    server.advance(EXPIRY - Duration::from_secs(1));
    let host_b = server.registry().install("host");
    let result = other.host.cleanup_leased_helper(&connection, &host_b.id, &helper.id, helper.global_id.as_ref().unwrap()).await;
    assert_eq!(result.unwrap(), false, "a live helper is reported busy, not removed");
    let registry = server.registry();
    assert!(registry.deletes.is_empty());
    assert!(registry.libraries.contains_key(&helper.id));
}

// 9
#[tokio::test]
async fn two_runs_compete_and_only_one_holds_a_helper_until_its_lease_ends() {
    let server = Server::new();
    let connection = connection(&server).await;
    let (first, second) = (case(), case());
    let shared = server.registry().install("host");
    first.host.keep_helper_alive(&first.run, &connection, &shared, true, &CancellationToken::new()).await.unwrap();
    let refused = second.host.keep_helper_alive(&second.run, &connection, &shared, true, &CancellationToken::new()).await.unwrap_err();
    assert_eq!(refused.code, "SERVER_LEASE_FAILED");
    assert!(second.host.server_leases.lock().await.is_empty(), "the refused run holds nothing");
    first.host.server_leases.lock().await.clear(); // the holder stops renewing
    server.advance(EXPIRY + Duration::from_secs(1));
    second.host.keep_helper_alive(&second.run, &connection, &shared, true, &CancellationToken::new()).await.unwrap();
    assert_eq!(server.registry().resources[shared.global_id.as_ref().unwrap()]["operation"], run_id(&second).as_str());
}

// 10
#[tokio::test]
async fn release_returns_the_lease_and_stops_the_heartbeat() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    case.host.release_server_lease(&case.run, &connection).await.unwrap();
    assert!(case.host.server_leases.lock().await.is_empty());
    assert!(!server.registry().live(&run_id(&case)));
    assert!(case.host.lease_gate(&case.run).await.unwrap().is_none(), "no lease, no gate");
    // Released resources are claimable at once, without waiting for expiry.
    let claim = server.registry().handle(&json!({"action":"claim","globalId":host_identity.global_id})).unwrap();
    assert_eq!(claim["resources"].as_array().unwrap().len(), 1);
}

// 11
#[tokio::test]
async fn release_after_the_server_expired_the_lease_is_reported() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    server.advance(EXPIRY + Duration::from_secs(1));
    let error = case.host.release_server_lease(&case.run, &connection).await.unwrap_err();
    assert_eq!(error.code, "SERVER_LEASE_FAILED");
    assert!(case.host.server_leases.lock().await.is_empty(), "the local keeper is gone either way");
}

// 12
#[tokio::test]
async fn lost_lease_refuses_new_helper_work_but_owned_cleanup_still_reaps() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    let helper = server.registry().install("plugin");
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    case.host.keep_helper_alive(&case.run, &connection, &helper, false, &CancellationToken::new()).await.unwrap();
    lost_flag(&case, &case.host).store(true, Ordering::SeqCst);
    server.advance(EXPIRY + Duration::from_secs(1)); // the server has expired it too
    assert_eq!(case.host.lease_gate(&case.run).await.unwrap_err().code, "SERVER_LEASE_LOST");
    let another = server.registry().install("plugin");
    assert_eq!(case.host.keep_helper_alive(&case.run, &connection, &another, false, &CancellationToken::new()).await.unwrap_err().code, "SERVER_LEASE_LOST");
    // Cleanup of this run's own helper is not new work.
    assert!(as_cleanup(case.host.lease_gate(&case.run)).await.unwrap().is_none());
    let removed = as_cleanup(case.host.cleanup_leased_helper(&connection, &host_identity.id, &helper.id, helper.global_id.as_ref().unwrap())).await.unwrap();
    assert!(removed);
    assert_eq!(server.registry().deletes, vec![helper.id.clone()]);
    assert!(!case.run.cancel.is_cancelled(), "losing the lease never cancels the run");
}

// 13
#[tokio::test]
async fn recognized_leftover_of_an_earlier_version_is_removed() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    let old = server.registry().install("plugin");
    server.registry().packages.insert(old.id.clone(), helper_package(&old.id, false));
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    let kept = case.host.reap_leftovers(&case.run, &connection, Some(&host_identity.id), vec![leftover(&old)], &CancellationToken::new()).await;
    assert!(kept.is_empty(), "the recognized leftover is not kept");
    assert_eq!(server.registry().deletes, vec![old.id.clone()]);
    assert!(case.run.storage.lock().unwrap().journal.diagnostics.iter().all(|d| d.code != "CLEANUP_AUTHORITY_REQUIRED"));
}

// 14
#[tokio::test]
async fn unrecognized_or_prefix_only_leftovers_are_kept_and_reported() {
    let server = Server::new();
    let connection = connection(&server).await;
    let case = case();
    let host_identity = server.registry().install("host");
    let changed = server.registry().install("plugin");
    let unreadable = server.registry().install("plugin"); // Herbert's name and notes, no matching contents
    server.registry().packages.insert(changed.id.clone(), helper_package(&changed.id, true));
    case.host.keep_helper_alive(&case.run, &connection, &host_identity, true, &CancellationToken::new()).await.unwrap();
    let kept = case.host.reap_leftovers(&case.run, &connection, Some(&host_identity.id), vec![leftover(&changed), leftover(&unreadable)], &CancellationToken::new()).await;
    assert_eq!(kept.iter().map(|helper| (helper.library_id.as_str(), helper.state.as_str())).collect::<Vec<_>>(),
        vec![(changed.id.as_str(), "unrecognized"), (unreadable.id.as_str(), "unrecognized")]);
    assert!(server.registry().deletes.is_empty());
    let reported = case.run.storage.lock().unwrap().journal.diagnostics.iter().filter(|d| d.code == "CLEANUP_AUTHORITY_REQUIRED").count();
    assert_eq!(reported, 2);
}

// 15
#[tokio::test]
async fn recognized_leftover_under_a_live_lease_is_kept() {
    let server = Server::new();
    let connection = connection(&server).await;
    let (owner, other) = (case(), case());
    let host_a = server.registry().install("host");
    let helper = server.registry().install("plugin");
    server.registry().packages.insert(helper.id.clone(), helper_package(&helper.id, false));
    owner.host.keep_helper_alive(&owner.run, &connection, &host_a, true, &CancellationToken::new()).await.unwrap();
    owner.host.keep_helper_alive(&owner.run, &connection, &helper, false, &CancellationToken::new()).await.unwrap();
    let host_b = server.registry().install("host");
    other.host.keep_helper_alive(&other.run, &connection, &host_b, true, &CancellationToken::new()).await.unwrap();
    let kept = other.host.reap_leftovers(&other.run, &connection, Some(&host_b.id), vec![leftover(&helper)], &CancellationToken::new()).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].state, "active");
    let registry = server.registry();
    assert!(registry.deletes.is_empty());
    assert!(registry.libraries.contains_key(&helper.id));
}

// 16
#[test]
fn the_runs_own_plugin_helpers_and_lease_host_are_never_reported_as_foreign() {
    let helper = |library: &str, generation: &str| crate::storage::ForeignPendingHelper {
        library_id: library.into(), global_id: generation.into(), role: "plugin".into(), state: "unknown".into() };
    let own_generation = uuid::Uuid::new_v4();
    let own = BTreeSet::from(["Hown0000000000000000000000000000".to_string(), own_generation.to_string()]);
    let kept = exclude_own(vec![
        helper("Hown0000000000000000000000000000", &uuid::Uuid::new_v4().to_string()), // own record, by library ID
        helper("Hother000000000000000000000000000", &own_generation.to_string().to_uppercase()), // own record, by generation
        helper("Hhost00000000000000000000000000", &uuid::Uuid::new_v4().to_string()), // this run's lease host
        helper("Hforeign0000000000000000000000000", "9c336e68-0000-0000-0000-000000000000"), // someone else's
    ], &own, Some("Hhost00000000000000000000000000"));
    assert_eq!(kept.iter().map(|h| h.library_id.as_str()).collect::<Vec<_>>(), ["Hforeign0000000000000000000000000"]);
}
