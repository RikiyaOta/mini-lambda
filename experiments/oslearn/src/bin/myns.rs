use nix::{
    libc::getuid,
    sched::{self, CloneFlags},
    sys::wait::waitpid,
    unistd::{ForkResult, execv, execvp, fork, getpid},
};

fn main() -> nix::Result<()> {
    println!("Pid before unshare: {}", getpid());

    // CLONE_NEWPID だけだと失敗するらしい？
    sched::unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWUSER)?;

    // すぐに今の pid を確認する。
    println!("Pid after unshare: {}", getpid());

    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
            println!("Pid after fork by Parent: {} (child={})", getpid(), child);

            // NOTE: 子プロセスを sleep させてる間に /proc/self/ns をみたいだけなので雑にしておく。
            waitpid(child, None).unwrap();
        }
        Ok(ForkResult::Child) => {
            println!("Pid after fork by Child: {}", getpid());
            println!("uid after fork by Child: {}", unsafe { getuid() });

            // NOTE: 子プロセスを sleep させてる間に /proc/self/ns をみたいだけなので雑にしておく。
            execvp(c"sleep", &[c"sleep", c"12345"]).unwrap();
        }
        Err(err) => {
            panic!("[Error] fork に失敗しました: {err}");
        }
    }

    Ok(())
}
