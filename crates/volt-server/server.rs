use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};

use tokio::{
    fs::{self, File, create_dir_all},
    io::{AsyncWriteExt, BufWriter},
    net::TcpListener,
};

use anyhow::{Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc};
use tokio_util::io::ReaderStream;
use tracing::{error, info, warn};

#[derive(Clone)]
struct AppState {
    config: ServerConfig,
}

#[derive(Clone, Deserialize)]
struct ServerConfig {
    auth_token: String,
    cache_dir: PathBuf,
    address: String,
}

async fn auth_middleware(State(state): State<Arc<AppState>>, request: Request<Body>, next: Next) -> Result<Response, StatusCode> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| {
            warn!("Missing or malformed Authorization header");
            StatusCode::UNAUTHORIZED
        })?;

    if auth_header != state.config.auth_token {
        warn!("Invalid authentication token provided");
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(request).await)
}

async fn logging_middleware(request: Request<Body>, next: Next) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let start = std::time::Instant::now();

    info!(%method, %uri, "Request started");
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let duration = start.elapsed();

    info!(
        %method,
        %uri,
        %status,
        duration_ms = duration.as_millis(),
        "Request completed"
    );

    response
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).with_target(false).init();

    let config: ServerConfig = toml::from_str(&tokio::fs::read_to_string("config.toml").await?)?;
    let state = Arc::new(AppState { config: config.clone() });
    let addr = config.address.parse::<SocketAddr>().with_context(|| format!("Failed to parse address: {}", config.address))?;

    print_startup_message(&addr, &config);

    let app = Router::new()
        .route("/health/{volt_id}", get(health))
        .route("/push/{volt_id}", post(push))
        .route("/pull/{volt_id}", get(pull))
        .route("/check/{volt_id}", get(check_hash))
        .layer(middleware::from_fn(logging_middleware))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(ExitCode::SUCCESS)
}

fn print_startup_message(addr: &SocketAddr, config: &ServerConfig) {
    const BOX_WIDTH: usize = 60;

    fn pad_line(content: &str) -> String { format!("║ {:<BOX_WIDTH$} ║", content) }

    info!(
        r#"
╔══════════════════════════════════════════════════════════════╗
║ started volt server :3                                       ║
╠══════════════════════════════════════════════════════════════╣
║                                                              ║
{}
{}
{}
║                                                              ║
╚══════════════════════════════════════════════════════════════╝
        "#,
        pad_line(&format!("listening on:     {}", addr)),
        pad_line(&format!("cache directory:  {:?}", config.cache_dir)),
        pad_line("authentication:   always on"),
    );
}

async fn health(Path(volt_id): Path<String>) -> String { volt_id }

async fn check_hash(Path(volt_id): Path<String>, State(state): State<Arc<AppState>>, headers: HeaderMap) -> Result<impl IntoResponse, StatusCode> {
    uuid::Uuid::parse_str(&volt_id).map_err(|e| {
        warn!("Invalid UUID format: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let client_hash = headers.get("X-Volt-Hash").and_then(|h| h.to_str().ok());
    let server_hash_path = state.config.cache_dir.join(format!("{volt_id}.hash"));
    let server_hash = tokio::fs::read_to_string(&server_hash_path).await.ok();

    info!("Hash check: client={client_hash:?} server={server_hash:?}");

    match (client_hash, server_hash) {
        (Some(client_hash), Some(server_hash)) => {
            if client_hash == server_hash.trim() {
                Ok(StatusCode::NOT_MODIFIED.into_response())
            } else {
                Ok(StatusCode::OK.into_response())
            }
        }
        (_, None) => Ok(StatusCode::NOT_FOUND.into_response()),
        (None, _) => {
            warn!("Missing X-Volt-Hash header");
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

async fn push(Path(volt_id): Path<String>, State(state): State<Arc<AppState>>, headers: HeaderMap, body: Body) -> Result<(), StatusCode> {
    uuid::Uuid::parse_str(&volt_id).map_err(|e| {
        warn!("Invalid UUID format: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    create_dir_all(&state.config.cache_dir).await.map_err(|e| {
        error!("Failed to create cache directory: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let file_path = state.config.cache_dir.join(format!("{}.zst", volt_id));
    let file = File::create(&file_path).await.map_err(|e| {
        error!("Failed to create file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut writer = BufWriter::new(file);
    let mut stream = body.into_data_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            error!("Stream error: {}", e);
            StatusCode::BAD_REQUEST
        })?;

        writer.write_all(&chunk).await.map_err(|e| {
            error!("Write error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    }

    writer.flush().await.map_err(|e| {
        error!("Flush error: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let hash = headers.get("X-Volt-Hash").and_then(|h| h.to_str().ok()).unwrap_or_default();
    let hash_path = state.config.cache_dir.join(format!("{}.hash", volt_id));

    fs::write(hash_path, hash).await.map_err(|e| {
        error!("Failed to write hash file: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(())
}

async fn pull(Path(volt_id): Path<String>, State(state): State<Arc<AppState>>, headers: HeaderMap) -> Result<impl IntoResponse, StatusCode> {
    uuid::Uuid::parse_str(&volt_id).map_err(|e| {
        warn!("Invalid UUID format: {}", e);
        StatusCode::BAD_REQUEST
    })?;

    let client_hash = headers.get("X-Volt-Hash").and_then(|h| h.to_str().ok());
    let server_hash_path = state.config.cache_dir.join(format!("{}.hash", volt_id));
    let server_hash = tokio::fs::read_to_string(&server_hash_path).await.ok();

    info!("{client_hash:?} to {server_hash:?}");

    if let (Some(client_hash), Some(server_hash)) = (client_hash, server_hash) {
        if client_hash == server_hash.trim() {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        }
    }

    let file_path = state.config.cache_dir.join(format!("{}.zst", volt_id));
    let file = File::open(&file_path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            warn!("File not found: {}", volt_id);
            StatusCode::NOT_FOUND
        } else {
            error!("File open error: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    let stream = ReaderStream::new(file);
    let mut headers = HeaderMap::new();
    headers.insert("Content-Encoding", "zstd".parse().unwrap());

    Ok((headers, Body::from_stream(stream)).into_response())
}
