use std::env::VarError;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use faas_core::{Function, Instance, InstanceMetrics, Sandbox};
use tokio::io::AsyncWriteExt;
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
            let script = self.function.base_dir.join(&self.function.manifest.handler);
            // 子プロセスでスクリプトを実行する.
            // 子は親から渡されるデータを標準入力で受け取って、その標準出力を親に渡すようにしたいので、
            // `Stdio::piped()` を使って親側からデータの読み書きができるようにする。
            let mut child = Command::new(script.into_os_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut child_stdin = child.stdin.take().unwrap();

            child_stdin.write_all(payload).await?;

            // 子の stdin を閉じなければ、書き込み側の fd が残っているとみなされて、
            // 子プロセスが終了してくれない。なので明示的に閉じておく。
            drop(child_stdin);

            let output = child.wait_with_output().await?;
            Ok(output.stdout)
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use anyhow::Result;
    use faas_core::{Function, FunctionManifest, Runtime};
    use tempfile::tempdir;

    fn prepare_function_toml(f: &Function) -> Result<()> {
        let path = f.base_dir.join("manifest.toml");
        let content = toml::to_string(&f.manifest)?;

        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, content)?;

        Ok(())
    }

    fn prepare_function_handler(f: &Function, handler: String) -> Result<()> {
        let path = f.base_dir.join(&f.manifest.handler);

        std::fs::create_dir_all(path.parent().unwrap())?;

        std::fs::write(&path, handler)?;

        // スクリプトに実行権限を付与しておく.
        let mut perm = std::fs::metadata(&path)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm)?;

        Ok(())
    }

    #[tokio::test]
    async fn can_pass_stdin_to_stdout_by_cat() -> Result<()> {
        // 関数定義とスクリプトの準備
        let name = "test_cat_001".to_string();
        let base_dir = tempdir()?;
        // 素直にファイル直書きでいいか？ごちゃついている。
        let f = Function {
            base_dir: base_dir.path().join(format!("functions/{}/", &name)),
            name: name.clone(),
            manifest: FunctionManifest {
                handler: "index".to_string(),
                runtime: Runtime::Script,
                timeout_ms: 12345,
                memory_mib: 67890,
                env: vec![(String::from("ENV1"), String::from("hoge"))]
                    .into_iter()
                    .collect(),
            },
        };
        let handler = "#!/bin/bash\ncat".to_string(); // cat をそのまま実行するだけのスクリプト.
        prepare_function_toml(&f)?;
        prepare_function_handler(&f, handler)?;

        // スクリプト実行
        let f = Function::build(&name, base_dir.path())?;
        let sandbox = SandboxProcess {};
        let mut instance = sandbox.spawn(&f).await?;
        let payload = "hoge fuga".as_bytes();
        let result = instance.invoke(payload).await?;

        // 検証
        assert_eq!(result, payload);

        Ok(())
    }
}
