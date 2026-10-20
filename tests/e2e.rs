use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post};
use serde_json::{Value, json};
use sq_system_one::{
    config::Config,
    daemon,
    engine::Engine,
    protocol::{Question, Request},
};
use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Default)]
struct Counts {
    active: Arc<AtomicUsize>,
    max: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}
async fn mock(State(counts): State<Counts>, Json(req): Json<Request>) -> axum::response::Response {
    counts.calls.fetch_add(1, Ordering::SeqCst);
    if req.model.as_deref() == Some("fail") {
        return (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response();
    }
    if req.model.as_deref() == Some("auth") {
        return (StatusCode::UNAUTHORIZED, "secret should not appear").into_response();
    }
    let active = counts.active.fetch_add(1, Ordering::SeqCst) + 1;
    counts.max.fetch_max(active, Ordering::SeqCst);
    let index = req.state.as_u64().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(if index == 0 { 80 } else { 5 })).await;
    counts.active.fetch_sub(1, Ordering::SeqCst);
    let answers: BTreeMap<_, _> = req.questions.iter().map(|(id, q)| {
        let answer = match q {
            Question::Choice { criteria, .. } => {
                let label = criteria.keys().next().unwrap();
                let p: BTreeMap<_, _> = criteria.keys().map(|k| (k.clone(), if k == label { 0.9 } else { 0.1 / (criteria.len() - 1) as f64 })).collect();
                json!({"type":"choice","choice": label,"probabilities":p,"confidence":0.8})
            }
            Question::Score { criteria, .. } => {
                let p: BTreeMap<_, _> = (0..criteria.len()).map(|i| (i.to_string(), if i == criteria.len()-1 { 1.0 } else { 0.0 })).collect();
                json!({"type":"score","score":criteria.len()-1,"probabilities":p,"confidence":0.95,"legend":criteria})
            }
            Question::Noul { .. } => json!({"type":"noul","noul":if index % 2 == 0 { 0.9 } else { 0.1 }}),
        };
        (id.clone(), answer)
    }).collect();
    Json(json!({"model":req.model.unwrap_or("auto".into()),"answers":answers,"usage":{"input_tokens":1,"output_tokens":0}})).into_response()
}

async fn server() -> (String, Counts) {
    let counts = Counts::default();
    let app = Router::new()
        .route("/v1/systemone", post(mock))
        .with_state(counts.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, counts)
}
fn request() -> Request {
    Request {
        state: json!("hello"),
        questions: BTreeMap::from([(
            "decision".into(),
            Question::Noul {
                instructions: "Urgent?".into(),
                criteria: None,
            },
        )]),
        model: None,
    }
}
fn pool(url: &str, primary: &str) -> Config {
    toml::from_str(&format!(
        r#"
provider = "primary"
fallback = ["secondary"]
retries = 0
concurrency = 2
[providers.primary]
base_url = "{url}"
model = "{primary}"
[providers.secondary]
base_url = "{url}"
model = "ok"
"#
    ))
    .unwrap()
}

#[tokio::test]
async fn fallback_and_auth_failure_policy() {
    let (url, counts) = server().await;
    let engine = Engine::new(&pool(&url, "fail")).unwrap();
    let out = engine.evaluate(request()).await.unwrap();
    assert_eq!(out["_sq"]["provider"], "secondary");
    assert_eq!(out["model"], "ok");
    assert_eq!(counts.calls.load(Ordering::SeqCst), 2);
    let engine = Engine::new(&pool(&url, "auth")).unwrap();
    let error = engine.evaluate(request()).await.unwrap_err().to_string();
    assert!(error.contains("HTTP 401"));
    assert!(!error.contains("secret"));
    assert_eq!(counts.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn round_robin_rotates_primary_and_validates_requests() {
    let (url, _) = server().await;
    let mut config = pool(&url, "ok");
    config.strategy = sq_system_one::config::Strategy::RoundRobin;
    let engine = Engine::new(&config).unwrap();
    assert_eq!(
        engine.evaluate(request()).await.unwrap()["_sq"]["provider"],
        "primary"
    );
    assert_eq!(
        engine.evaluate(request()).await.unwrap()["_sq"]["provider"],
        "secondary"
    );
    let mut r = request();
    r.model = Some("some-model".into());
    assert!(engine.evaluate(r).await.is_err());
}

async fn cli(url: String, args: Vec<&'static str>, input: &'static str) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        let config = tempfile::NamedTempFile::new().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_sq"))
            .args(["--base-url", &url, "--retries", "0", "-j", "3"])
            .args(args)
            .env("SQ_CONFIG", config.path())
            .env_remove("TYPESAFE_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn cli_preserves_order_and_concurrency_with_thresholds() {
    let (url, counts) = server().await;
    let out = cli(url.clone(), vec!["--noul", "Urgent?"], "0\n1\n2\n3\n4\n5\n").await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|r| r["index"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5]
    );
    assert!(counts.max.load(Ordering::SeqCst) > 1);
    assert!(counts.max.load(Ordering::SeqCst) <= 3);
    let out = cli(
        url.clone(),
        vec![
            "--noul",
            "Urgent?",
            "--threshold",
            "0.8",
            "--emit-input",
            "-r",
        ],
        "0\n1\n2\n",
    )
    .await;
    assert!(out.status.success());
    assert_eq!(out.stdout, b"0\n2\n");
    let out = cli(
        url.clone(),
        vec!["--choice", "normal,critical", "--confidence-gating", "0.9"],
        "hello\n",
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let out = cli(
        url.clone(),
        vec![
            "--score",
            "Severity",
            "--levels",
            "Low|High",
            "--score-scale",
            "100",
            "--threshold",
            "80",
            "-r",
        ],
        "hello\n",
    )
    .await;
    assert!(out.status.success());
    assert_eq!(out.stdout, b"100.0\n");
    let out = cli(
        url,
        vec!["--noul", "Urgent?", "--confidence-gating", "0.8"],
        "hello\n",
    )
    .await;
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_unix_socket_relays_and_preserves_existing_socket() {
    use std::os::unix::fs::PermissionsExt;
    let (url, _) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("relay.sock");
    let mut config = pool(&url, "ok");
    config.fallback.clear();
    config.daemon.socket = Some(socket.clone());
    let copy = config.clone();
    let handle = tokio::spawn(daemon::serve(config, true));
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(daemon::serve(copy, true).await.is_err());
    let client = reqwest::Client::builder()
        .unix_socket(socket.clone())
        .build()
        .unwrap();
    let response = client
        .post("http://localhost/v1/systemone")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    handle.abort();
    let _ = handle.await;
    assert!(!socket.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_auth_admission_body_limit_and_sigterm_cleanup() {
    let (url, counts) = server().await;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("authenticated.sock");
    let mut config = pool(&url, "ok");
    config.concurrency = 1;
    config.fallback.clear();
    config.daemon.socket = Some(socket.clone());
    config.daemon.token_env = Some("SQ_TEST_DAEMON_TOKEN".into());
    config.daemon.max_body_bytes = 1024;
    let path = temp.path().join("config.toml");
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_sq"))
            .args(["--config", path.to_str().unwrap(), "daemon", "-q"])
            .env("SQ_TEST_DAEMON_TOKEN", "test-relay-key")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(socket.exists());
    let client = reqwest::Client::builder()
        .unix_socket(socket.clone())
        .build()
        .unwrap();
    let url = "http://localhost/v1/systemone";
    let unauthorized = client
        .post(url)
        .header("content-type", "application/json")
        .body("not json")
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        client
            .post(url)
            .bearer_auth("test-relay-key")
            .header("content-type", "application/json")
            .body("x".repeat(2048))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let first_client = client.clone();
    let first = tokio::spawn(async move {
        first_client
            .post(url)
            .bearer_auth("test-relay-key")
            .json(&request())
            .send()
            .await
            .unwrap()
    });
    for _ in 0..100 {
        if counts.active.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let second = client
        .post(url)
        .bearer_auth("test-relay-key")
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second.headers()["retry-after"], "1");
    assert!(first.await.unwrap().status().is_success());
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    for _ in 0..100 {
        if child.0.try_wait().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(child.0.try_wait().unwrap().unwrap().success());
    assert!(!socket.exists());
}

#[tokio::test]
async fn jsonl_errors_can_continue_and_state_file_infers_json() {
    let (url, _) = server().await;
    let out = cli(
        url.clone(),
        vec![
            "--noul",
            "Urgent?",
            "--input-format",
            "jsonl",
            "--on-error",
            "emit",
        ],
        "not-json\n0\n",
    )
    .await;
    assert_eq!(out.status.code(), Some(2));
    let rows: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(rows[0].get("error").is_some());
    assert_eq!(rows[1]["noul"], 0.9);
    let out = cli(
        url.clone(),
        vec!["--noul", "Urgent?", "-r", "--output-format", "response"],
        "hello\n",
    )
    .await;
    assert_eq!(out.status.code(), Some(2));
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("tickets.json");
    let config = temp.path().join("config.toml");
    std::fs::write(&input, "[\n  0,\n  1\n]\n").unwrap();
    std::fs::write(&config, "").unwrap();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_sq"))
            .args([
                "--config",
                config.to_str().unwrap(),
                "--base-url",
                &url,
                "--noul",
                "Urgent?",
                "--state-file",
                input.to_str().unwrap(),
            ])
            .env_remove("TYPESAFE_API_KEY")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().lines().count(), 2);
}
