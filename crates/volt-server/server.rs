use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};

use tokio::{
    fs::{create_dir_all, File},
    io::AsyncWriteExt,
    net::TcpListener,
};

use futures::StreamExt;
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
struct ServerConfig {
    auth_token: String,
    cache_dir: PathBuf,
}

#[derive(Clone)]
struct AppState {
    config: ServerConfig,
}

async fn auth_middleware(State(state): State<Arc<AppState>>, request: Request<Body>, next: Next) -> Result<Response, StatusCode> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if auth_header != state.config.auth_token {
        return Err(StatusCode::FORBIDDEN);
    }

    Ok(next.run(request).await)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config: ServerConfig = toml::from_str(&tokio::fs::read_to_string("config.toml").await?)?;
    let state = Arc::new(AppState { config });

    let app = Router::new()
        .route("/push/{volt_id}", post(push))
        .route("/pull/{volt_id}", get(pull))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = TcpListener::bind(addr).await?;
    println!("Server listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn push(Path(volt_id): Path<String>, State(state): State<Arc<AppState>>, body: Body) -> Result<(), StatusCode> {
    Uuid::parse_str(&volt_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    create_dir_all(&state.config.cache_dir).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let file_path = state.config.cache_dir.join(format!("{}.zst", volt_id));
    let mut file = File::create(&file_path).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
        file.write_all(&chunk).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    Ok(())
}

async fn pull(Path(volt_id): Path<String>, State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, StatusCode> {
    Uuid::parse_str(&volt_id).map_err(|_| StatusCode::BAD_REQUEST)?;

    let file_path = state.config.cache_dir.join(format!("{}.zst", volt_id));
    let file = File::open(&file_path).await.map_err(|_| StatusCode::NOT_FOUND)?;

    let stream = ReaderStream::new(file);
    let mut headers = HeaderMap::new();
    headers.insert("Content-Encoding", "zstd".parse().unwrap());

    Ok((headers, Body::from_stream(stream)))
}
