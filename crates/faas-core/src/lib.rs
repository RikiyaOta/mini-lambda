//! FaaS の中核となる型: `Function`(manifest)と、隔離方式を差し替えるための `Sandbox` / `Instance` trait。

mod function;
use anyhow::Result;
pub use function::{Function, FunctionManifest, Runtime};
use std::future::Future;

/// 実行環境の起動内訳. RSS など.
///
/// NOTE: 必要になったタイミングで具体的な項目は追加していく.
pub struct InstanceMetrics {}

/// 隔離された実行環境の作り方の定義.
pub trait Sandbox: Send + Sync {
    type Instance: Instance;

    fn spawn(&self, f: &Function) -> impl Future<Output = Result<Self::Instance>> + Send;
}

/// `Sandbox` によって実際に作られた実行環境.
pub trait Instance: Send {
    /// 実行環境が作られた関数を呼び出す.
    fn invoke(&mut self, payload: &[u8]) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// 実行環境を破棄する.
    fn destroy(self) -> impl Future<Output = Result<()>> + Send;

    /// 実行環境の中身を観察する.
    fn metrics(&self) -> InstanceMetrics;
}
