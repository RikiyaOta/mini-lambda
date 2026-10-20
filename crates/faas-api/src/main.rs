//! HTTP 層: `POST /invoke/{function_name}` を受けて `Sandbox` に渡す。
//!
//! - リクエストボディがそのまま payload になり、関数の stdout がそのままレスポンスボディになる
//! - Phase 1 では 1 リクエストごとに `spawn → invoke → destroy` する(毎回コールドスタート。プールは Phase 5)
//! - 関数は `{MINI_LAMBDA_BASE_DIR}/functions/{name}/manifest.toml` から読む(既定はカレントディレクトリ)
//!
//! エージェントが書いた部分(CLAUDE.md の「書いてよいもの」)。

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use faas_core::{Function, Instance, Sandbox};
use sandbox_process::SandboxProcess;
use tokio::time::error::Elapsed;

/// ハンドラから共有して使うもの。リクエストをまたいで読むだけなので `Arc` で包むだけでよい(`Mutex` 不要)
struct AppState<S> {
    sandbox: S,
    base_dir: PathBuf,
}

/// ルーターを組み立てる。`S` を差し替えれば別の隔離バックエンドで同じ API が動く
fn app<S: Sandbox + 'static>(sandbox: S, base_dir: PathBuf) -> Router {
    let state = Arc::new(AppState { sandbox, base_dir });
    Router::new()
        .route("/invoke/{function_name}", post(invoke::<S>))
        .with_state(state)
}

async fn invoke<S: Sandbox>(
    State(state): State<Arc<AppState<S>>>,
    Path(name): Path<String>,
    payload: Bytes,
) -> Response {
    // `..` などで functions/ の外を読まれないよう、名前に使える文字を絞る
    if !is_valid_name(&name) {
        return (StatusCode::BAD_REQUEST, "invalid function name\n").into_response();
    }

    let f = match Function::build(&name, &state.base_dir) {
        Ok(f) => f,
        Err(e) if is_not_found(&e) => {
            return (StatusCode::NOT_FOUND, format!("function not found: {name}\n")).into_response();
        }
        Err(e) => return internal_error("build", e),
    };

    let mut instance = match state.sandbox.spawn(&f).await {
        Ok(i) => i,
        Err(e) => return internal_error("spawn", e),
    };

    let result = instance.invoke(&payload).await;

    // 結果に関わらず片付ける。失敗してもレスポンスは返す(ログだけ残す)
    if let Err(e) = instance.destroy().await {
        eprintln!("[{name}] destroy failed: {e:#}");
    }

    match result {
        Ok(out) => (StatusCode::OK, out).into_response(),
        // sandbox-process は `tokio::time::timeout` の `Elapsed` を anyhow に包んで返す
        Err(e) if e.is::<Elapsed>() => {
            (StatusCode::GATEWAY_TIMEOUT, format!("function timed out: {name}\n")).into_response()
        }
        Err(e) => internal_error("invoke", e),
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn internal_error(stage: &str, e: anyhow::Error) -> Response {
    eprintln!("{stage} failed: {e:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{stage} failed: {e:#}\n")).into_response()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_dir = std::env::var_os("MINI_LAMBDA_BASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let addr: SocketAddr = std::env::var("MINI_LAMBDA_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string())
        .parse()?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("listening on http://{addr} (base_dir = {})", base_dir.display());
    axum::serve(listener, app(SandboxProcess {}, base_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tempfile::{TempDir, tempdir};
    use tower::ServiceExt;

    /// `functions/{name}/` に manifest と実行可能な handler を置く
    fn put_function(base: &TempDir, name: &str, timeout_ms: u32, script: &str) {
        let dir = base.path().join(format!("functions/{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = format!(
            "handler = \"index\"\nruntime = \"script\"\ntimeout_ms = {timeout_ms}\nmemory_mib = 128\nenv = {{}}\n"
        );
        std::fs::write(dir.join("manifest.toml"), manifest).unwrap();
        let handler = dir.join("index");
        std::fs::write(&handler, script).unwrap();
        std::fs::set_permissions(&handler, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// サーバーを立てずに、ルーターへ直接 1 リクエスト流す。
    ///
    /// 本番では `axum::serve` がソケット(`bind` / `accept` / `read` / `write`)と HTTP の解析を担当し、
    /// 組み立てた `Request` を `Router` に渡している。`Router` 自体は「`Request` → `Response`」の関数のようなもので、
    /// ネットワークを知らない。ここではその真ん中だけを `oneshot`(`tower::ServiceExt`)で 1 回呼ぶ。
    ///
    /// ポートを開かないので、並列に走るテスト同士でポートが衝突せず、起動・停止の待ちも要らない。
    /// TCP を含めた通しの確認は、`curl` で手動、またはベンチ(`bench/`)で行う。
    async fn post(base: &TempDir, name: &str, body: &'static [u8]) -> (StatusCode, Vec<u8>) {
        let req = Request::post(format!("/invoke/{name}"))
            .body(Body::from(body))
            .unwrap();
        let res = app(SandboxProcess {}, base.path().to_path_buf())
            .oneshot(req)
            .await
            .unwrap();
        let status = res.status();
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn returns_stdout_of_function() {
        let base = tempdir().unwrap();
        put_function(&base, "echo", 5000, "#!/bin/bash\ncat\n");

        let (status, body) = post(&base, "echo", b"hello").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"hello");
    }

    #[tokio::test]
    async fn unknown_function_is_404() {
        let base = tempdir().unwrap();

        let (status, _) = post(&base, "nope", b"").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_name_is_400() {
        let base = tempdir().unwrap();

        let (status, _) = post(&base, "..", b"").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn timeout_is_504() {
        let base = tempdir().unwrap();
        put_function(&base, "slow", 200, "#!/bin/bash\nsleep 23456\n");

        let (status, _) = post(&base, "slow", b"").await;

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    }
}
