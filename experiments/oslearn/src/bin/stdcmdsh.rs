use std::process::Command;

/// myrun.rs との比較をするためだけのスクリプト
fn main() {
    Command::new("sh")
        .args(["-c", "echo hi; sleep 10"])
        .output()
        .expect("[Error] stdcmdsh が予期せぬエラーで失敗."); // エラーハンドリングはここでは興味ないので雑に済ませる。
}
