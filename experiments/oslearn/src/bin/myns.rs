use std::{io::ErrorKind, num::NonZeroUsize, path::Path, ptr};

use libseccomp::{ScmpAction, ScmpFilterContext, ScmpSyscall};
use nix::{
    libc::EPERM,
    mount::{self, MntFlags, MsFlags, umount2},
    sched::{self, CloneFlags},
    sys::{
        mman::{MapFlags, ProtFlags, mmap_anonymous},
        wait::waitpid,
    },
    unistd::{ForkResult, chdir, execvp, fork, getgid, getpid, getuid, pivot_root},
};

fn main() -> nix::Result<()> {
    print_ids("before unshare");

    let current_uid = getuid();
    let current_gid = getgid();

    // 新しい User namespace を作り、その中にネストさせて Pid namespace を作る。
    // 現在のプロセスはその新しい User namespace の中に入る。
    // CLONE_NEWNS は歴史的な名残でこうなっているが、Mount namespace を作るフラグ。
    sched::unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)?;

    // すぐに今の pid を確認する。
    // この時点では対応表がないので、uid は変わっているはず。
    print_ids("after unshare");

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
    print_ids("after writing into /proc/self/uid_map");

    // /proc/self/gid_map に書き込みができるようにするためには、
    // 事前に /proc/[pid]/setgroups ファイルの中身を "deny" (文字列)にしておく必要がある。
    println!(
        "Content of /proc/self/setgroups before updating it: {}",
        std::fs::read_to_string("/proc/self/setgroups").unwrap()
    );
    std::fs::write("/proc/self/setgroups", "deny").unwrap();

    std::fs::write("/proc/self/gid_map", format!("0 {current_gid} 1")).unwrap();
    print_ids("after writing into /proc/self/gid_map");

    let crgoup_root =
        Path::new("/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/mini-lambda");

    // このスクリプトを度々動かしているので、カウンタをリセットするために、cgroup を削除しておく。
    // ※ 最初にスクリプトを動かすときはまだディレクトリがないので、そのエラーだけは無視しておく。
    if let Err(e) = std::fs::remove_dir(crgoup_root)
        && e.kind() != ErrorKind::NotFound
    {
        panic!("cgroup の削除に失敗しました: {e}");
    }

    // cgroup を使って、リソースを制限する。
    // まず新しいグループを作る(単にディレクトリを作るだけ)。
    std::fs::create_dir_all(crgoup_root).unwrap();
    // メモリの上限を設定するために `memory.max`, `memory.swap.max` に書き込む。
    std::fs::write(crgoup_root.join("memory.max"), "10M").unwrap();
    std::fs::write(crgoup_root.join("memory.swap.max"), "0").unwrap();
    // プロセス数の上限を設定する。
    std::fs::write(crgoup_root.join("pids.max"), "5").unwrap();
    // cpu の上限をかける（ 0.1sec ごとに 最大 0.01sec まで、つまり CPU1️⃣個の 10% までという制限）
    // 注意：数値はマイクロ秒単位
    std::fs::write(crgoup_root.join("cpu.max"), "10000 100000").unwrap();

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            let pid = getpid();
            let uid = getuid();
            let gid = getgid();
            println!(
                "---------------------------
pid after fork by Parent: {pid} (child={child})
uid after fork by Parent: {uid}
gid after fork by Parent: {gid}
"
            );

            let result = waitpid(child, None).unwrap();
            println!("[parent] wait status: {:?}", result);
        }
        Ok(ForkResult::Child) => {
            print_ids("after fork by Child");

            // pivot_root で、子プロセスの `/` を `mini-lambda/guest/out/rootfs/` に切り替える。
            let new_root =
                Path::new("/home/rikiyaota/dev/github.com/RikiyaOta/mini-lambda/guest/out/rootfs/");
            // 元の rootfs を退避させる場所。あとで unmount する。子は pivot_root のあと new_root 配下しかみれないので、new_root 直下に作っておく。
            let put_old = new_root.join("old-root");
            std::fs::create_dir_all(&put_old).unwrap();

            // 子プロセスをこの新しいグループに登録するために、cgroups.procs に書き込む。
            // cgroup_root は pivot_root + umount で見えなくなるので、その前に実施する必要がある。
            std::fs::write(
                crgoup_root.join("cgroup.procs"),
                // NOTE: 書き込んだプロセスから見たPIDを書き込めばOK。
                // NOTE: このプロセスが Pid namespace の中にいても、プロセスはその属するnamespaceとその中でのPIDを表すデータをカーネルは知っているので。
                getpid().as_raw().to_string(),
            )
            .unwrap();

            // pivot_root では、new_root が mount point であることを要求する。
            // そこで、ハックみたいなやり方だが、自分自身に bind mount をすることで解決する。
            mount::mount(
                Some(new_root),
                new_root,
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            )
            .unwrap();
            pivot_root(new_root, &put_old).unwrap();

            // あとで `sh -c "sleep ..."` をするときに `/dev/null` が必要になる。
            // 子からみえる /dev/null もホストと同じものを使うことにする。
            // /old-root/dev/null にホストの /dev/null が見えているので、そこを使う。
            // 注意：上で pivot_root をしてるので、ルートが変わっていることに注意。ホストのルートは
            // /old-root に見えている。
            let new_dev_null = "/dev/null";
            let host_dev_null = "/old-root/dev/null";
            mount::mount(
                Some(host_dev_null),
                new_dev_null,
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            )
            .unwrap();

            // 新しく /proc をマウントし直す
            mount::mount(
                Some("proc"),
                "/proc",
                Some("proc"),
                MsFlags::empty(),
                None::<&str>,
            )
            .unwrap();

            // 新しい `/` に移動する。
            chdir("/").unwrap();

            // `put_old` を unmount する.
            // これで子プロセスから、元の rootfs、つまりホストのルートディレクトリが見えなくなった！
            //
            // MNT_DETACH : /old-root がまだ他のプロセスに使われている可能性があるので、「今すぐ木から切り離して、使っている人がいなくなったら片付ける」という指定。
            umount2("/old-root", MntFlags::MNT_DETACH).unwrap();

            // 大きくメモリを確保して書き込む処理を実施し、リソース制限が効いているか確認する。
            // `mymem.rs` でやったように、mmap して書き込む処理をする。
            // write_huge_memory(1 << 25);
            // 確認：子プロセスが実際に使っている物理メモリを確認する。
            // print_vmsize_vmrss(None);

            // seccomp の検証のため、デフォルトで全ての syscall
            // を許可し、１つだけ禁止するルールを定義する。
            let mut ctx = ScmpFilterContext::new(ScmpAction::Allow).unwrap();
            let syscall = ScmpSyscall::from_name("mkdir").unwrap(); // 今回は mkdir を禁止する
            ctx.add_rule(ScmpAction::Errno(EPERM), syscall).unwrap();
            ctx.load().unwrap(); // カーネルにこのフィルターを取り付ける。

            // 確認：`/` がホストのものと異なっていることを確認する。
            // execvp(c"ls", &[c"ls", c"-l", c"/"]).unwrap();
            //
            // 確認：子プロセスから見えるプロセス一覧を見てみる。
            // execvp(c"ps", &[c"ps"]).unwrap();
            //
            // 確認：プロセス数の制限が効いていることをみるため、複数プロセスを起動してみる。
            // execvp( c"sh", &[c"sh", c"-c", c"for i in $(seq 1 10); do sleep 777 & done"],) .unwrap();
            //
            // 確認：CPUを使い続けるプロセスを起動して制限がかかることを確かめる(終わらないのも困るので、busybox の timeout だけ入れておく)
            //      ---> ここで１つ発見と注意。`sh` は pid=1　でそのまま実行される。init process
            //      には、init process
            //      自身がシグナルハンドラーを定義したシグナルしか送れないのでtimeout処理が完了せずにずっと止まらない。
            //      namespace の外側からは殺せるので、単に Ctrl-C で止めることはできる。
            // execvp(
            //     c"timeout",
            //     &[
            //         c"timeout",
            //         c"5",
            //         c"sh",
            //         c"-c",
            //         c"echo $$; while :; do :; done",
            //     ],
            // )
            // .unwrap();
            //
            // 確認：seccomp で `mkdir` だけを禁止したので、そのフィルターが効いているか検証する
            execvp(
                c"sh",
                &[
                    c"sh",
                    c"-c",
                    c"mkdir /tmp/x; echo \"exit=$?\"; grep Seccomp /proc/self/status",
                ],
            )
            .unwrap();
        }
        Err(err) => {
            panic!("[Error] fork に失敗しました: {err}");
        }
    }

    Ok(())
}

fn print_ids(suffix: &str) {
    let pid = getpid();
    let uid = getuid();
    let gid = getgid();
    println!(
        "---------------------------
pid {suffix}: {pid}
uid {suffix}: {uid}
gid {suffix}: {gid}
"
    );
}

/// 指定されたサイズのメモリを `mmap` して書き込むことでページフォルトを発生させて
/// 物理メモリを消費する。メモリ制限が効くか確かめるためだけの処理。
fn write_huge_memory(memory_size: usize) {
    unsafe {
        let addr = mmap_anonymous(
            None,
            NonZeroUsize::new(memory_size).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();
        let page_count = memory_size / 4096;
        let base: *mut u8 = addr.as_ptr().cast();
        for i in 0..page_count {
            ptr::write(base.add(i * 4096), 1);
        }
    }
}

fn print_vmsize_vmrss(prefix: Option<String>) {
    let prefix = prefix.unwrap_or("".to_string());
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .split("\n")
        .filter(|line| !line.is_empty())
        .for_each(|line| {
            if line.starts_with("VmSize:") || line.starts_with("VmRSS:") {
                println!("{prefix}{line}");
            }
        });
}
