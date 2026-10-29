use nix::{
    mount::{self, MntFlags, MsFlags, umount2},
    sched::{self, CloneFlags},
    sys::wait::waitpid,
    unistd::{ForkResult, chdir, execvp, fork, getgid, getpid, getuid, pivot_root},
};

fn main() -> nix::Result<()> {
    println!("---------------------------");
    println!("Pid before unshare: {}", getpid());
    println!("uid before unshare: {}", getuid());
    println!("gid before unshare: {}", getgid());

    let current_uid = getuid();
    let current_gid = getgid();

    // 新しい User namespace を作り、その中にネストさせて Pid namespace を作る。
    // 現在のプロセスはその新しい User namespace の中に入る。
    // CLONE_NEWNS は歴史的な名残でこうなっているが、Mount namespace を作るフラグ。
    sched::unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)?;

    println!("---------------------------");
    // すぐに今の pid を確認する。
    println!("Pid after unshare: {}", getpid());
    println!("uid after unshare: {}", getuid()); // この時点では対応表がないので、uid
    // は変わっているはず。
    println!("gid after unshare: {}", getgid());

    // fork の前に /proc/self/uid_map に対応表を１つ書き込む。
    // /proc/self/uid_map が、自分から見えている user namespace
    // における対応表を見せる窓になっている。
    // また、親子が同じ user namespace に属するので、同じ対応表を見ることになる。
    //
    // /proc/self/uid_map のフォーマット
    // ```
    // <中のuid> <外のuid> <個数>
    // ```
    //
    // /proc/self/gid_map のフォーマット
    // ```
    // <中のgid> <外のgid> <個数>
    // ```
    std::fs::write("/proc/self/uid_map", format!("0 {current_uid} 1")).unwrap();
    println!("---------------------------");
    println!("Pid after writing into /proc/self/uid_map: {}", getpid());
    println!("uid after writing into /proc/self/uid_map: {}", getuid());
    println!("gid after writing into /proc/self/uid_map: {}", getgid());

    // /proc/self/gid_map に書き込みができるようにするためには、
    // 事前に /proc/[pid]/setgroups ファイルの中身を "deny" (文字列)にしておく必要がある。
    println!(
        "Content of /proc/self/setgroups before updating it: {}",
        std::fs::read_to_string("/proc/self/setgroups").unwrap()
    );
    std::fs::write("/proc/self/setgroups", "deny").unwrap();

    std::fs::write("/proc/self/gid_map", format!("0 {current_gid} 1")).unwrap();
    println!("---------------------------");
    println!("Pid after writing into /proc/self/gid_map: {}", getpid());
    println!("uid after writing into /proc/self/gid_map: {}", getuid());
    println!("gid after writing into /proc/self/gid_map: {}", getgid());

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            println!("---------------------------");
            println!("Pid after fork by Parent: {} (child={})", getpid(), child);
            println!("uid after fork by Parent: {}", getuid());
            println!("gid after fork by Parent: {}", getgid());

            waitpid(child, None).unwrap();
        }
        Ok(ForkResult::Child) => {
            println!("---------------------------");
            println!("Pid after fork by Child: {}", getpid());
            println!("uid after fork by Child: {}", getuid());
            println!("gid after fork by Child: {}", getgid());

            mount::mount(
                Some("proc"),
                "/proc",
                Some("proc"),
                MsFlags::empty(),
                None::<&str>,
            )
            .unwrap();
            // /proc をマウントし直せているか確認する。
            // execvp(c"ps", &[c"ps"]).unwrap();

            // pivot_root で、子プロセスの `/` を `mini-lambda/guest/out/rootfs/` に切り替える。
            let new_root = "/home/rikiyaota/dev/github.com/RikiyaOta/mini-lambda/guest/out/rootfs/";
            // 元の rootfs を退避させる場所。あとで unmount する。子は pivot_root のあと new_root 配下しかみれないので、new_root 直下に作っておく。
            let put_old =
                "/home/rikiyaota/dev/github.com/RikiyaOta/mini-lambda/guest/out/rootfs/old-root";
            std::fs::create_dir_all(put_old).unwrap();
            pivot_root(new_root, put_old).unwrap();

            // 新しい `/` に移動する。
            chdir("/").unwrap();

            // `put_old` を unmount する.
            // これで子プロセスから、元の rootfs、つまりホストのルートディレクトリが見えなくなった！
            //
            // MNT_DETACH : /old-root がまだ他のプロセスに使われている可能性があるので、「今すぐ木から切り離して、使っている人がいなくなったら片付ける」という指定。
            umount2("/old-root", MntFlags::MNT_DETACH).unwrap();

            // 確認：`/` がホストのものと異なっていることを確認する。
            execvp(c"ls", &[c"ls", c"-l", c"/"]).unwrap();
        }
        Err(err) => {
            panic!("[Error] fork に失敗しました: {err}");
        }
    }

    Ok(())
}
