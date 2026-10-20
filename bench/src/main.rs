//! ベンチハーネス: コールドスタートの p50 / p99 と段階ごとの内訳を測って results/ に CSV で残す。
//!
//! 使い方(どちらも release で測る):
//!
//! ```text
//! cargo run --release -p bench -- stages [N]   # Sandbox を直接呼び、build / spawn / invoke / destroy の内訳
//! cargo run --release -p faas-api &            # 別ターミナルでサーバーを起動しておく
//! cargo run --release -p bench -- http [N]     # HTTP 経由。最初のバイトまで(TTFB)と全体
//! ```
//!
//! - 対象の関数は `functions/echo`(`cat` するだけ)。payload は小さな固定文字列
//! - 最初の `WARMUP` 回は捨てる(ページキャッシュやバイナリの読み込みが温まる前の外れ値を除くため)
//! - 生データは `results/phase1-{stages,http}.csv`、要約は標準出力と `results/phase1.csv`
//! - 測ったマシン(CPU 名とカーネル)を要約 CSV に一緒に残す
//!
//! エージェントが書いた部分(CLAUDE.md の「書いてよいもの」)。

use std::{
    fs,
    io::Write as _,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use faas_core::{Function, Instance, Sandbox};
use sandbox_process::SandboxProcess;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const FUNCTION: &str = "echo";
const PAYLOAD: &[u8] = b"hello";
const WARMUP: usize = 10;
const DEFAULT_N: usize = 200;
const BACKEND: &str = "process";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let n: usize = match args.get(2) {
        Some(s) => s.parse().context("N には回数を指定してください")?,
        None => DEFAULT_N,
    };

    fs::create_dir_all("results")?;
    match mode {
        "stages" => bench_stages(n).await,
        "http" => bench_http(n).await,
        _ => bail!("usage: bench <stages|http> [N]"),
    }
}

/// `Sandbox` を直接呼んで、段階ごとの時間を測る(HTTP 層を含まない)
async fn bench_stages(n: usize) -> Result<()> {
    let sandbox = SandboxProcess {};
    let base = Path::new(".");
    let mut rows: Vec<[Duration; 5]> = Vec::with_capacity(n);

    for i in 0..WARMUP + n {
        let t0 = Instant::now();
        let f = Function::build(FUNCTION, base)?;
        let t1 = Instant::now();
        let mut instance = sandbox.spawn(&f).await?;
        let t2 = Instant::now();
        let out = instance.invoke(PAYLOAD).await?;
        let t3 = Instant::now();
        instance.destroy().await?;
        let t4 = Instant::now();

        if out != PAYLOAD {
            bail!("unexpected output: {:?}", String::from_utf8_lossy(&out));
        }
        if i >= WARMUP {
            rows.push([t1 - t0, t2 - t1, t3 - t2, t4 - t3, t4 - t0]);
        }
    }

    let names = ["build", "spawn", "invoke", "destroy", "total"];
    let mut csv = String::from("backend,iter,build_us,spawn_us,invoke_us,destroy_us,total_us\n");
    for (i, r) in rows.iter().enumerate() {
        let cols: Vec<String> = r.iter().map(|d| d.as_micros().to_string()).collect();
        csv += &format!("{BACKEND},{i},{}\n", cols.join(","));
    }
    fs::write("results/phase1-stages.csv", csv)?;

    let summary: Vec<(String, Vec<Duration>)> = names
        .iter()
        .enumerate()
        .map(|(k, name)| (format!("stages.{name}"), rows.iter().map(|r| r[k]).collect()))
        .collect();
    report(&summary)
}

/// サーバー(`faas-api`)に HTTP で投げて、最初のバイトまで(TTFB)と全体を測る。
///
/// 1 リクエストごとに新しい TCP 接続を張り、`Connection: close` で閉じてもらう(curl を毎回叩くのに近い)。
/// HTTP クライアントのクレートを使わず生の TCP で書いているのは、「最初のバイトが届いた瞬間」を自分で取るため。
async fn bench_http(n: usize) -> Result<()> {
    let addr = std::env::var("MINI_LAMBDA_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let request = format!(
        "POST /invoke/{FUNCTION} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        PAYLOAD.len()
    );
    let mut rows: Vec<[Duration; 2]> = Vec::with_capacity(n);

    for i in 0..WARMUP + n {
        let t0 = Instant::now();
        let mut stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("{addr} に接続できません。faas-api を起動してください"))?;
        stream.set_nodelay(true)?;
        stream.write_all(request.as_bytes()).await?;
        stream.write_all(PAYLOAD).await?;

        let mut buf = vec![0u8; 4096];
        let first = stream.read(&mut buf).await?;
        let t1 = Instant::now();
        let mut response = buf[..first].to_vec();
        stream.read_to_end(&mut response).await?;
        let t2 = Instant::now();

        if !response.starts_with(b"HTTP/1.1 200") || !response.ends_with(PAYLOAD) {
            bail!("unexpected response:\n{}", String::from_utf8_lossy(&response));
        }
        if i >= WARMUP {
            rows.push([t1 - t0, t2 - t0]);
        }
    }

    let mut csv = String::from("backend,iter,ttfb_us,total_us\n");
    for (i, r) in rows.iter().enumerate() {
        csv += &format!("{BACKEND},{i},{},{}\n", r[0].as_micros(), r[1].as_micros());
    }
    fs::write("results/phase1-http.csv", csv)?;

    report(&[
        ("http.ttfb".to_string(), rows.iter().map(|r| r[0]).collect()),
        ("http.total".to_string(), rows.iter().map(|r| r[1]).collect()),
    ])
}

/// p50 / p99 を表示し、`results/phase1.csv` に追記する(同じ metric の古い行は置き換える)
fn report(metrics: &[(String, Vec<Duration>)]) -> Result<()> {
    let machine = machine();
    let path = "results/phase1.csv";
    let header = "backend,metric,n,p50_us,p99_us,machine";

    // 既存の行のうち、今回測っていない metric だけ残す
    let mut lines: Vec<String> = fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter(|l| !metrics.iter().any(|(m, _)| l.starts_with(&format!("{BACKEND},{m},"))))
        .map(String::from)
        .collect();

    println!("{:<16} {:>6} {:>10} {:>10}", "metric", "n", "p50_us", "p99_us");
    for (name, samples) in metrics {
        let (p50, p99) = (percentile(samples, 0.50), percentile(samples, 0.99));
        println!("{name:<16} {:>6} {p50:>10} {p99:>10}", samples.len());
        lines.push(format!("{BACKEND},{name},{},{p50},{p99},\"{machine}\"", samples.len()));
    }
    lines.sort();

    let mut f = fs::File::create(path)?;
    writeln!(f, "{header}")?;
    for l in lines {
        writeln!(f, "{l}")?;
    }
    println!("machine: {machine}");
    Ok(())
}

/// nearest-rank 法のパーセンタイル(µs)
fn percentile(samples: &[Duration], p: f64) -> u128 {
    let mut v: Vec<u128> = samples.iter().map(Duration::as_micros).collect();
    v.sort_unstable();
    let rank = ((p * v.len() as f64).ceil() as usize).max(1);
    v[rank - 1]
}

fn machine() -> String {
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| "unknown cpu".to_string());
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    format!("{cpu} / Linux {kernel}")
}
