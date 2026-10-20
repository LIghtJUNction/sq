use crate::{
    config::{Config, default_socket},
    engine::Engine,
    protocol::Request,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
struct App {
    engine: Arc<Engine>,
    token: Option<String>,
    capacity: Arc<Semaphore>,
}

type ApiResult = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;

fn error(code: StatusCode, message: String) -> (StatusCode, Json<Value>) {
    (code, Json(json!({"error": {"message": message}})))
}

async fn admission(
    State(app): State<App>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/health" {
        return next.run(request).await;
    }
    if let Some(token) = &app.token {
        let actual = request
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let expected = format!("Bearer {token}");
        // Compare all bytes without stopping at the first mismatching byte.
        let diff = actual
            .bytes()
            .zip(expected.bytes())
            .fold(actual.len() ^ expected.len(), |d, (a, b)| {
                d | usize::from(a ^ b)
            });
        if diff != 0 {
            return error(
                StatusCode::UNAUTHORIZED,
                "invalid daemon bearer token".into(),
            )
            .into_response();
        }
    }
    let Ok(_slot) = app.capacity.clone().try_acquire_owned() else {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "daemon is at capacity".into(),
        )
        .into_response();
        response
            .headers_mut()
            .insert("retry-after", "1".parse().unwrap());
        return response;
    };
    next.run(request).await
}

async fn evaluate(State(app): State<App>, Json(request): Json<Request>) -> ApiResult {
    request
        .validate()
        .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    app.engine
        .evaluate(request)
        .await
        .map(Json)
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e.to_string()))
}

pub async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn serve(config: Config, quiet: bool) -> Result<()> {
    let engine = Arc::new(Engine::new(&config)?);
    let token = match config.daemon.token_env.as_ref() {
        Some(name) => {
            let t = std::env::var(name)
                .with_context(|| format!("daemon token environment variable {name} is missing"))?;
            ensure!(
                !t.trim().is_empty() && t.bytes().all(|b| (32..=126).contains(&b)),
                "daemon token must be nonblank ASCII without control characters"
            );
            Some(t)
        }
        None => None,
    };
    let state = App {
        engine,
        token: token.clone(),
        capacity: Arc::new(Semaphore::new(config.concurrency)),
    };
    let app = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/v1/systemone", post(evaluate))
        .layer(DefaultBodyLimit::max(config.daemon.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), admission))
        .with_state(state);
    if let Some(address) = config.daemon.listen {
        ensure!(
            address.ip().is_loopback() || token.is_some(),
            "non-loopback TCP requires daemon.token_env"
        );
        let listener = tokio::net::TcpListener::bind(address).await?;
        if !quiet {
            eprintln!("sq daemon listening on http://{}", listener.local_addr()?);
        }
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown())
            .await?;
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let path = config.daemon.socket.unwrap_or_else(default_socket);
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)?;
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
                }
            }
            let listener = tokio::net::UnixListener::bind(&path).with_context(|| {
                format!(
                    "cannot bind {}; existing sockets are never removed automatically",
                    path.display()
                )
            })?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            struct SocketGuard {
                path: std::path::PathBuf,
                inode: u64,
            }
            impl Drop for SocketGuard {
                fn drop(&mut self) {
                    if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
                        let _ = std::fs::remove_file(&self.path);
                    }
                }
            }
            let _guard = SocketGuard {
                inode: std::fs::symlink_metadata(&path)?.ino(),
                path: path.clone(),
            };
            if !quiet {
                eprintln!("sq daemon listening on unix://{}", path.display());
            }
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        #[cfg(not(unix))]
        anyhow::bail!("Unix sockets are unavailable; use sq daemon --listen 127.0.0.1:7979");
    }
    Ok(())
}
