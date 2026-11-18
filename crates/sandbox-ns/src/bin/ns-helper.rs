//! `sandbox-ns` のヘルパー。サーバーから別プロセスとして起動され、箱を作ってから関数のハンドラーを実行する。
//!
//! シングルスレッドで動くこと(tokio を使わない)。`unshare(CLONE_NEWUSER)` はマルチスレッドだと EINVAL になる
//! (`experiments/oslearn/src/bin/myns_in_multithread.rs` で確認済み)。
//!
//! 中身は `experiments/oslearn/src/bin/myns.rs` でやったことを移していく。作者が書く。

use std::{
    env,
    ffi::CString,
    fs,
    io::{Error, ErrorKind},
    path::Path,
};

use nix::{
    mount::{self, MntFlags, MsFlags, umount2},
    sched::{self, CloneFlags},
    sys::wait::waitpid,
    unistd::{ForkResult, chdir, execv, fork, getgid, getpid, getuid, pivot_root},
};

// 関数定義とか rootfs とかおいてあるディレクトリのパス
const RESOURCE_ROOT: &str = "/home/rikiyaota/dev/github.com/RikiyaOta/mini-lambda";

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    match (args.next(), args.next(), args.next()) {
        (Some(rootfs), Some(func_dir), Some(handler)) => {
            println!("rootfs: {rootfs}");
            println!("func_dir: {func_dir}");
            println!("handler: {handler}");

            // 引数検証はいったんOK。まあ存在確認はあとで。
            // PathBuf で存在チェック簡単にできるかも？
            // stdin を読む。
            // 親プロセスのものをそのまま引き継いで使えば十分なので、自分の stdin を素直に読む。
            // ---> あれ？そもそもヘルパーで読む必要がないのか？スクリプトがやればいい？
            // let mut buf: Vec<u8> = vec![];
            // stdin().read_to_end(&mut buf)?;

            // これ以降のやることはまさに `myns.rs` と同じはず。
            // unshare して、cgroup, seccomp とか設定して、fork+execv

            // NOTE: unshare する前に uid, gid を控えておく。
            let uid = getuid();
            let gid = getgid();

            sched::unshare(
                CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS,
            )?;

            // User namespace における uid, gid のマッピング
            fs::write("/proc/self/uid_map", format!("0 {uid} 1"))?;
            fs::write("/proc/self/setgroups", "deny")?;
            fs::write("/proc/self/gid_map", format!("0 {gid} 1"))?;

            // cgroup の定義（これは外に出すことは可能。あとで検討）
            // `myns.rs` のときは実行前にcgroupを毎回削除していたけど、今回は作成済みならそれはそれでいいので、削除自体はやめておく。
            // 不要になったときの削除は `Instance::destroy` の責務だと判断する。
            //
            // リソースの制限値については関数定義に書き込んでおくはず。
            let crgoup_root = Path::new(
                "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/mini-lambda/ns-helper",
            );
            fs::create_dir_all(crgoup_root)?;
            fs::write(crgoup_root.join("memory.max"), "10M")?; // FIXME: ここは manifest.toml から読み取る必要がある。
            fs::write(crgoup_root.join("memory.swap.max"), "0")?;
            fs::write(crgoup_root.join("pids.max"), "2")?; // NOTE: 特に指定はないけどいったん固定値にしておく。

            match unsafe { fork() }? {
                ForkResult::Parent { child } => {
                    // 単に子プロセスを待って終了するだけにする。
                    // stdout への書き込みは子プロセスにやらせる。
                    waitpid(child, None)?;
                }
                ForkResult::Child => {
                    let resource_root = Path::new(RESOURCE_ROOT);
                    let new_root = resource_root.join(rootfs);
                    let old_root = new_root.join("old-root");
                    fs::create_dir_all(&old_root)?;
                    fs::write(
                        crgoup_root.join("cgroup.procs"),
                        getpid().as_raw().to_string(),
                    )?;
                    mount::mount(
                        Some(&new_root),
                        &new_root,
                        None::<&str>,
                        MsFlags::MS_BIND,
                        None::<&str>,
                    )?;
                    pivot_root(&new_root, &old_root)?;
                    let new_dev_null = "/dev/null";
                    let host_dev_null = "/old-root/dev/null";
                    mount::mount(
                        Some(host_dev_null),
                        new_dev_null,
                        None::<&str>,
                        MsFlags::MS_BIND,
                        None::<&str>,
                    )?;
                    mount::mount(
                        Some("proc"),
                        "/proc",
                        Some("proc"),
                        MsFlags::empty(),
                        None::<&str>,
                    )?;
                    // func_dir も見えるようにする必要がある。
                    // /old-dir/${resource_root}/${func_dir} に見えているはず。
                    let new_func_dir = "/function";
                    let host_func_dir = Path::new("/old-root").join(RESOURCE_ROOT).join(&func_dir);
                    // FIXME: 関数ディレクトリは読み取り専用にしておいたほうが安全。`man 2 mount` の
                    // Creating a bind mount と Remounting on exisiting mount
                    // を読んで読み取り専用にする方法を調べる。
                    mount::mount(
                        Some(&host_func_dir),
                        new_func_dir,
                        None::<&str>,
                        MsFlags::MS_BIND,
                        None::<&str>,
                    )?;

                    chdir("/")?;
                    umount2("/old-root", MntFlags::MNT_DETACH)?;

                    // TODO: seccomp の何かしらのフィルターを追加？

                    // 関数ハンドラーを実行する.
                    // `manifest.toml` の `runtime` に応じた起動をしないといけないはず。
                    // 一旦、`runtime: script` を対象とする。
                    // あとやること：ハンドラーを受け取って、ハンドラーのスクリプトを実行する。
                    let script_path = Path::new("/function")
                        .join(&func_dir)
                        .join(handler)
                        .into_string()
                        .unwrap();
                    println!("script_path: {script_path}");
                    let script = CString::new(script_path)?;
                    execv(&script, &[&script])?;
                }
            }

            Ok(())
        }
        _ => Err(Error::new(ErrorKind::InvalidInput, "引数が足りません。"))?,
    }
}
