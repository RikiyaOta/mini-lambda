//! Phase 2 のバックエンド: 関数を namespace + cgroup + seccomp の箱の中で実行する。
//!
//! サーバー(tokio、マルチスレッド)は自分では `unshare` できないので、
//! 箱を作る処理はヘルパー `ns-helper` に任せる。ここではヘルパーを起動して、
//! stdin / stdout をパイプでつなぐ(`sandbox-process` と同じ形)。

use std::path::PathBuf;

use anyhow::Result;
use faas_core::{Function, Instance, InstanceMetrics, Sandbox};

/// 関数を namespace + cgroup + seccomp の箱の中で実行する
pub struct SandboxNs {
    /// ヘルパー `ns-helper` の実行ファイルのパス
    pub helper: PathBuf,
    /// 箱の `/` にする rootfs(`guest/build-rootfs.sh` で作ったもの)
    pub rootfs: PathBuf,
}

impl Sandbox for SandboxNs {
    type Instance = InstanceNs;

    fn spawn(&self, f: &Function) -> impl Future<Output = Result<Self::Instance>> + Send {
        let _ = f;
        async move { todo!() }
    }
}

/// `SandboxNs` のインスタンス
pub struct InstanceNs {}

impl Instance for InstanceNs {
    fn invoke(&mut self, payload: &[u8]) -> impl Future<Output = Result<Vec<u8>>> + Send {
        let _ = payload;
        async move { todo!() }
    }

    fn destroy(self) -> impl Future<Output = Result<()>> + Send {
        async move { todo!() }
    }

    fn metrics(&self) -> InstanceMetrics {
        InstanceMetrics {}
    }
}
