use std::process::Stdio;
use std::time::Duration;

use anyhow::{Error, Result};
use faas_core::{Function, Instance, InstanceMetrics, Sandbox};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        async {
            let script = self.function.base_dir.join(&self.function.manifest.handler);
            // 子プロセスでスクリプトを実行する.
            // 子は親から渡されるデータを標準入力で受け取って、その標準出力を親に渡すようにしたいので、
            // `Stdio::piped()` を使って親側からデータの読み書きができるようにする。
            let mut child = Command::new(script.into_os_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .process_group(0) // 子プロセス自身のPIDを PGID にする.
                .spawn()
                .unwrap();
            let child_pid = Pid::from_raw(child.id().unwrap() as i32);

            let future = async {
                let mut child_stdin = child.stdin.take().unwrap();
                let mut child_stdout = child.stdout.take().unwrap();

                let mut result: Vec<u8> = vec![];
                let write_future = async {
                    child_stdin.write_all(payload).await?;
                    // 書き込み側の fd が残っていると、子プロセスが終了してくれない。
                    // なので明示的に閉じておく (async move にしてもいいはず)
                    drop(child_stdin);
                    Ok::<(), Error>(())
                };
                let read_future = child_stdout.read_to_end(&mut result);
                // 子プロセス(bashなど)の終了を待ってから、残りの子孫を殺す
                // 孫プロセスがバックグラウンドで動き続けるなどして、stdout
                // を握ったまま関数全体が終了しないパターンを考慮する (read_future が終わらない)
                let reap_future = async {
                    let _ = child.wait().await?;
                    match killpg(child_pid, Signal::SIGKILL) {
                        // プロセスグループに誰もいないケース。
                        // これは正常終了扱いにする。
                        Err(Errno::ESRCH) => Ok::<(), Error>(()),
                        other => other.map_err(anyhow::Error::new),
                    }
                };

                let _ = tokio::join!(write_future, read_future, reap_future);

                Ok(result)
            };

            match timeout(
                Duration::from_millis(self.function.manifest.timeout_ms as u64),
                future,
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    // タイムアウトしたときにここに分岐する
                    // 子プロセスをルートとするプロセスグループ全体に SIGKILL を送る。
                    // (`myrun.rs` のときは最初にSIGTERMを送ったけど、今回は簡略化)
                    killpg(child_pid, Signal::SIGKILL)?;
                    Err(e)?
                }
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

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    use super::*;
    use anyhow::Result;
    use faas_core::Function;
    use tempfile::{TempDir, tempdir};

    /// テスト用にファイルを用意するヘルパー。
    /// ファイルの内容を直接文字列でかけるように簡潔なシグネチャにしておく。
    fn write_file(path: PathBuf, contents: String, is_executable: bool) -> Result<()> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, contents)?;

        if is_executable {
            let mut perm = std::fs::metadata(&path)?.permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&path, perm)?;
        }

        Ok(())
    }

    fn manifest_path(base: &TempDir, name: &str) -> PathBuf {
        base.path().join(format!("functions/{name}/manifest.toml"))
    }

    fn script_path(base: &TempDir, name: &str) -> PathBuf {
        base.path().join(format!("functions/{name}/index"))
    }

    #[tokio::test]
    async fn can_pass_stdin_to_stdout_by_cat() -> Result<()> {
        let base_dir = tempdir()?;

        // 関数定義とスクリプトの準備
        let name = "test_cat_001";
        let raw_manifest = r#"
handler = "index"
runtime = "script"
timeout_ms = 12345
memory_mib = 67890
env = { ENV1 = "hoge" }
"#
        .to_string();
        write_file(manifest_path(&base_dir, name), raw_manifest, false)?;

        let script_content = r#"#!/bin/bash

cat
"#
        .to_string();
        write_file(script_path(&base_dir, name), script_content, true)?;

        // スクリプト実行
        let f = Function::build(name, base_dir.path())?;
        let sandbox = SandboxProcess {};
        let mut instance = sandbox.spawn(&f).await?;
        let payload = b"hoge fuga";
        let result = instance.invoke(payload).await?;

        // 検証
        assert_eq!(result, payload);

        Ok(())
    }

    #[tokio::test]
    async fn can_pass_large_payload() -> Result<()> {
        let base_dir = tempdir()?;

        // 関数定義とスクリプトの準備
        let name = "test_cat_002";
        let raw_manifest = r#"
handler = "index"
runtime = "script"
timeout_ms = 12345
memory_mib = 67890
env = { ENV1 = "hoge" }
"#
        .to_string();
        write_file(manifest_path(&base_dir, name), raw_manifest, false)?;

        let script_content = r#"#!/bin/bash

cat
"#
        .to_string();
        write_file(script_path(&base_dir, name), script_content, true)?;

        // スクリプト実行
        let f = Function::build(name, base_dir.path())?;
        let sandbox = SandboxProcess {};
        let mut instance = sandbox.spawn(&f).await?;
        let payload = &[b'a'; 1 << 20]; // 1MB のデータを流す
        let result = instance.invoke(payload).await?;

        // 検証
        assert_eq!(result, payload);

        Ok(())
    }

    #[tokio::test]
    async fn cancel_child_by_timeout() -> Result<()> {
        let base_dir = tempdir()?;

        // 関数定義とスクリプトの準備
        let name = "test_cat_002";
        let raw_manifest = r#"
handler = "index"
runtime = "script"
timeout_ms = 200
memory_mib = 67890
env = { ENV1 = "hoge" }
"#
        .to_string();
        write_file(manifest_path(&base_dir, name), raw_manifest, false)?;

        let script_content = r#"#!/bin/bash

sleep 12345
"#
        .to_string();
        write_file(script_path(&base_dir, name), script_content, true)?;

        // スクリプト実行
        let f = Function::build(name, base_dir.path())?;
        let sandbox = SandboxProcess {};
        let mut instance = sandbox.spawn(&f).await?;
        let payload = b"hoge";
        let result = instance.invoke(payload).await;

        // 検証
        assert!(result.is_err_and(|result| result.to_string() == "deadline has elapsed"));

        // 子や孫プロセスが生きていないことを確認する
        let status = std::process::Command::new("pgrep")
            .args(["-f", "sleep 12345"])
            .status()?;
        assert!(!status.success()); // 一致するプロセスがないことが期待値.

        Ok(())
    }

    #[tokio::test]
    async fn kill_descendant_processes_in_happy_path() -> Result<()> {
        let base_dir = tempdir()?;

        // 関数定義とスクリプトの準備
        let name = "test_cat_003";
        let raw_manifest = r#"
handler = "index"
runtime = "script"
timeout_ms = 5000
memory_mib = 67890
env = { ENV1 = "hoge" }
"#
        .to_string();
        write_file(manifest_path(&base_dir, name), raw_manifest, false)?;

        // スクリプト自体が即座に成功するが、バックグラウンドでプロセスが残るケース
        let script_content = r#"#!/bin/bash

sleep 99999 & echo done
"#
        .to_string();
        write_file(script_path(&base_dir, name), script_content, true)?;

        // スクリプト実行
        let f = Function::build(name, base_dir.path())?;
        let sandbox = SandboxProcess {};
        let mut instance = sandbox.spawn(&f).await?;
        let payload = b"hoge";
        let result = instance.invoke(payload).await;

        // 検証
        // 関数自体は即座に成功しており、子孫プロセスがバックグラウンドにも残っていないことが期待値.
        assert!(result.is_ok_and(|result| String::from_utf8_lossy(&result) == "done\n"));
        let status = std::process::Command::new("pgrep")
            .args(["-f", "sleep 99999"])
            .status()?;
        assert!(!status.success()); // 一致するプロセスがないことが期待値.

        Ok(())
    }
}
