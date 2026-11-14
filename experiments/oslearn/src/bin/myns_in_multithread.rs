use std::{
    thread::{self, sleep},
    time::Duration,
};

use nix::sched::{self, CloneFlags};

/// ちょっとした実験のためだけのスクリプト。
/// user namespace を作るときには、unshare するプロセスはマルチスレッドが禁止されていることを確認する。
/// tokio ではマルチスレッドでサーバープロセスが動くので、これを回避するために、サンドボックスは別プロセスで起動する必要がある。
/// そのことを納得するために、実際に unshare がマルチスレッドでできないことを確認するためのスクリプト。
fn main() -> nix::Result<()> {
    let handler = thread::spawn(|| sleep(Duration::from_secs(3)));

    // スレッドが生きている間に unshare を実行する。
    sched::unshare(CloneFlags::CLONE_NEWUSER)?;

    handler.join().unwrap();

    Ok(())
}
