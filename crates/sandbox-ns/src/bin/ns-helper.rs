//! `sandbox-ns` のヘルパー。サーバーから別プロセスとして起動され、箱を作ってから関数のハンドラーを実行する。
//!
//! シングルスレッドで動くこと(tokio を使わない)。`unshare(CLONE_NEWUSER)` はマルチスレッドだと EINVAL になる
//! (`experiments/oslearn/src/bin/myns_in_multithread.rs` で確認済み)。
//!
//! 中身は `experiments/oslearn/src/bin/myns.rs` でやったことを移していく。作者が書く。

fn main() {
    todo!()
}
