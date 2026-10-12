use std::env::VarError;
use std::time::Duration;

use anyhow::Result;
use faas_core::{Function, Instance, InstanceMetrics, Sandbox};
use tokio::{fs, process::Command, time::timeout};

/// 関数を素のサブプロセスとして起動する（隔離なし。比較の基準）
pub struct SandboxProcess {}

impl Sandbox for SandboxProcess {
    type Instance = InstanceProcess;

    /// プロセスの起動自体は `Instance::invoke` で実行する。
    /// ここでは準備（handler のファイルがあるか？ `Function` の中身を Instance にコピーしておく、など）だけ実施。
    fn spawn(
        &self,
        f: &faas_core::Function,
    ) -> impl Future<Output = Result<Self::Instance>> + Send {
        async move {
            let handler_path = f.base_dir.join(f.manifest.handler.clone());
            let handler_content = fs::read_to_string(handler_path).await?;
            let function = (*f).clone();
            Ok(InstanceProcess {
                function,
                handler_content,
            })
        }
    }
}

/// `SandboxProcess` のインスタンス.
pub struct InstanceProcess {
    pub function: Function,

    /// ハンドラーの中身。
    /// ここではスクリプトの全文を保持しておく。
    /// `invoke` の際にはこのスクリプトをそのまま実行する。
    pub handler_content: String,
}

impl Instance for InstanceProcess {
    fn invoke(&mut self, payload: &[u8]) -> impl Future<Output = Result<Vec<u8>>> + Send {
        let future = async {
            let args = str::from_utf8(payload)?;
            // FIXME: "sh" じゃなくて、スクリプト自体を渡すべきか。
            // args はその引数と思うべきか？実行時に渡すものなので。
            let output = Command::new("sh").args(["-c", args]).output().await?;
            Ok(output.stdout) // 標準エラー出力を捨ててることになるか？
        };

        async {
            match get_timeout() {
                Some(secs) => timeout(Duration::from_secs(secs), future).await?,
                None => future.await,
            }
        }
    }

    fn destroy(self) -> impl Future<Output = Result<()>> + Send {
        // `invoke` の時に子プロセスを起動するだけなので、
        // 破棄すべきものは特にない。
        async { Ok(()) }
    }

    fn metrics(&self) -> InstanceMetrics {
        InstanceMetrics {}
    }
}

fn get_timeout() -> Option<u64> {
    match std::env::var("MINI_LAMBDA_TIMEOUT") {
        Ok(v) => Some(
            v.parse()
                .expect("[Error] MINI_LAMBDA_TIMEOUT には秒数を指定してください。"),
        ),
        Err(VarError::NotPresent) => None,
        Err(VarError::NotUnicode(_)) => {
            panic!("[Error] ここは到達しないはず。MYRUN_TIMEOUT は Unicode なので。")
        }
    }
}
