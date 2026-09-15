use nix::errno::Errno;
use nix::libc::_exit;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, killpg, sigaction};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::alarm::{cancel, set};
use nix::unistd::{ForkResult, Pid, dup2_stdout, execvp, fork, pipe, setpgid};
use std::env::VarError;
use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};

// 子プロセスにタイムアウトを設定するために用意したフラグ。
// 親プロセスに対して SIGALARM が発行されたことを検知するために使う。
static IS_SIGALARM_TRIGGERED: AtomicBool = AtomicBool::new(false);

fn main() -> nix::Result<()> {
    let mut args = std::env::args().skip(1);

    match args.next() {
        Some(cmd) => {
            // 子プロセスの　stdout をおやプロセスにリダイレクトするために使う。
            let (read_fd, write_fd) = pipe().expect("[Error] pipe に失敗しました。");

            // 以下は子プロセスのための準備だが、fork の前に実施する。
            // fork はプロセスをコピーするが、スレッドは自身しかコピーしないため、fork した後はすぐ exec をするのが無難。
            // メモリ確保するなど、他スレッドと干渉するような処理は避けた方が良いため、fork の前に準備しておく。

            // Rust の文字列(&str)は、ポインタであり、文字の長さも持っている。
            // システムコールのインターフェースはCの規約で決まっている。Cでは、文字列はただのポインタで、長さは持っていない( \0 (NULバイト) が終端というルール)
            // なので、Rust の &str をそのままでは渡せないので、型できちんと区別されている。
            let cmd = CString::new(cmd).expect("[Error] cmd を CString に変換できませんでした。");

            // 引数を CString に変換する。
            let mut args: Vec<_> = args
                .map(|s| CString::new(s).expect("[Error] 引数を String に変換できませんでした。"))
                .collect();

            // 引数の先頭はコマンド自身を指定する必要がある。
            args.insert(0, cmd.clone());

            match unsafe { fork() } {
                // この分岐が親プロセス側の処理と思われる.
                Ok(ForkResult::Parent { child }) => {
                    // 親プロセスは write_fd を使わないので明示的に閉じておく。
                    // なぜか？
                    // --------
                    // # 前提1
                    // `pipe` 直後は writer_fd は1つだけだが、`fork`は fd テーブルごとコピーをするため、
                    // 書き込み側の fd が親プロセス・子プロセスそれぞれに1つずつ、合計2つ存在することになる。
                    //
                    // # 前提2
                    // `read` が 0 を返す（読み込みが終了）条件は、カーネル側から見ると、そのパイプの書き込み側を
                    // 指定している fd がシステム全体で1つも残っていないこと。であるらしい。書き込み側のプロセスが終了したときではないのがポイント。
                    //
                    // # 結論
                    // なので、使わない fd があるなら、drop しないといけない。
                    // 持っていたら、「まだ書き込まれる可能性があるかも」という意思表示に（意図せず）なってしまう。
                    // --------
                    drop(write_fd);

                    // 環境変数 `MYRUN_TIMEOUT` が指定された場合は、指定された秒数後に親自身に SIGALARM を送るように設定する。
                    // SIGALARM を受け取った時に、ブロック中の `read` が中断されて `EINTR` を受け取るので、それを合図に子プロセスを kill する。
                    if let Some(timeout) = get_timeout() {
                        // シグナルハンドラを登録する。
                        // 具体的には、SIGALARM を受け取った時にカーネルがプロセスを一旦止めて実行する関数を登録する。
                        // カーネルが呼び出すので、`extern "C"` の形式で関数を定義する必要がある（CのABIで行われる）。
                        let _ = unsafe {
                            // これが今回定義するシグナルハンドラ.
                            extern "C" fn on_alarm(_signum: i32) {
                                // `read` が `EINTR` を返した時に、その `EINTR` が SIGALARM 由来のものだと
                                // 判断できるようにしてあげる必要がある。
                                //
                                // シグナルハンドラは、今まさに動いているスレッドを止めて、その上に割り込んで走る。
                                // そのため、もしシグナルハンドラが走る前に何かロックを獲得していて、シグナルハンドラでそのロックを待つような処理を書いてしまうと、
                                // ロックが解放されずにデッドロックになってしまう。この事情は、`fork` と `exec` と同様。
                                //
                                // `AtomicBool` は1命令で完了し、途中で止められる状態がないので、スレッド間でも安全だし、シグナル割り込みでも安全なので、
                                // ---- １命令で完了するってことについて ---
                                // AtomicBool は　CPU命令1個で処理できる。ハードウェアが、１バイトの書き込みが中途半端な状態で見えることがないと保証している。
                                // 一方で、`Mutex` を使うような、ロックを使いたい場合というのは、複数ステップの処理の途中の状態を他人に見せたくない場合に使う。これはソフトウェア側で守る仕組み。
                                // ------------------------------------
                                //
                                // シグナルハンドラ内でも安全に実行できる。
                                IS_SIGALARM_TRIGGERED.store(true, Ordering::Relaxed);
                            }
                            let handler = SigHandler::Handler(on_alarm);
                            let action = SigAction::new(handler, SaFlags::empty(), SigSet::empty());
                            sigaction(Signal::SIGALRM, &action)?
                        };

                        // すでに set されていれば、残り秒数を返すらしいが、そこまで興味ないので単に無視する。
                        let _ = set(timeout);
                    }

                    // 子プロセスがパイプに書き込んだデータを読み込んで出力する。
                    let mut buf = [0u8; 8192]; // バッファサイズは固定しておく。
                    let mut n = 0;
                    let mut bytes = Vec::new();

                    let mut is_first_sigalarm_triggered = false;

                    loop {
                        // `waitpid` の前に `read` する必要がある。
                        // なぜか？
                        // -----------
                        // # 前提
                        // パイプの容量は有限で、Linux は64KiB。パイプが一杯になると、書き手の write は親が読んで空きを作るまでブロックする。
                        //
                        // # 結論
                        // 仮に `waitpid` を `read`　より先に実行したら、以下のようになる：
                        // ```
                        // 子: 64KiB書いた -> パイプが満杯 -> write でブロック（親が読んでくれたら続きを書けるよ〜）
                        // 親: waitpid でブロック（子が終了してくれたら読むよ〜）
                        // ```
                        // つまり、デッドロック。64KiB に満たない小さな出力なら問題ないが、大量の出力になると上の状態になってしまう。
                        // なので、親側では先に　`read` する必要がある。
                        // -----------
                        match read(&read_fd, &mut buf)? {
                            ReadResult::Success(0) => {
                                // 子プロセスが書き込んだデータを全て読み取った。
                                println!(
                                    "[myrun] captured {} bytes: \"{}\"",
                                    n,
                                    String::from_utf8_lossy(&bytes[..n]).escape_debug()
                                );

                                // タイムアウト時の最初の SIGTERM で子プロセスが素直に死んだ場合、
                                // SIGKILL を送るようのアラームが残ったままになるので、EOF がきたタイミングで `cancel()` を実行しておく。
                                cancel();

                                break;
                            }
                            ReadResult::Success(m) => {
                                // 新しく読み取ったデータがある。
                                // 読み込んだバイト数は n に累積していく。
                                // 読み込んだデータは res に詰め込んでいく。最後に出力する必要があるため。
                                n += m;
                                bytes.extend_from_slice(&buf[..m]);
                            }
                            ReadResult::Timeout => {
                                if !is_first_sigalarm_triggered {
                                    // 子プロセスのタイムアウトになった。
                                    // タイムアウトになった旨のログ出力と、子プロセスの kill を実施する。
                                    println!(
                                        "[myrun] timeout ({}s), sending SIGTERM to pid={}",
                                        get_timeout().unwrap(), // Timeout になる場合はタイムアウトは指定されているはずなので、Noneになることは想定されない。
                                        child
                                    );
                                    killpg(child, Signal::SIGTERM)?;
                                    is_first_sigalarm_triggered = true;

                                    // SIGKILL までの猶予を set しないといけない。
                                    // false に戻しておかないと、SIGALARM以外の理由で read が EINTR を返した時にも Timeout になってしまう。
                                    IS_SIGALARM_TRIGGERED.store(false, Ordering::Relaxed);
                                    let _ = set(1); // 猶予の1秒は固定にする。
                                } else {
                                    // すでに SIGTERM を送ったが、まだ子プロセスが生きていた場合に受け取った SIGALARM.
                                    // 今度は SIGTERM ではなく、SIGKILL する。
                                    println!(
                                        "[myrun] still alive after 1s, sending SIGKILL to pid={child}"
                                    );
                                    killpg(child, Signal::SIGKILL)?;
                                }

                                // この後、以下2つを実施する必要がある:
                                // 1. パイプの EOF まで読み切ること。
                                // 2. `waitpid` をすること。
                                //
                                // 1 は、タイムアウト前に子プロセスがパイプに書き込んでいたデータが残っている可能性があるため。なので最後まで読み取る必要がある。
                                // なので、この分岐では `break` を呼ばずに次のループに移る。そうすれば、`read` が最後まで読み取って終了する。
                                //
                                // 2 は、子プロセスが正常に終了したことを判断しないと、ゾンビプロセスとして残ってしまう。
                                // 加えて、`waitpid` の結果を見て初めて、子プロセスが本当にSIGTERMで死んだのか、SIGTERMを無視して（trapして）生きていたのかが分かるから。
                            }
                        }
                    }

                    // 子プロセスの処理が完了するのを待つ。
                    match waitpid(child, None).expect("[Error] waitpid が失敗しました。") {
                        WaitStatus::Exited(pid, status) => {
                            println!("[myrun] pid={pid} exited with {status}");
                            exit(status);
                        }

                        // 第３引数はコアダンプを生成したかどうか。
                        // 補足：コアダンプとは、「死んだ瞬間のプロセスのメモリをファイルに書き出したもの」のこと。
                        //      シグナルでプロセスが死ぬのは予期しないことなので、後からデバッグができるようにカーネルが保存する。
                        WaitStatus::Signaled(pid, signal, _) => {
                            println!("[myrun] pid={} killed by signal {}", pid, signal);
                            exit(128 + signal as i32);
                        }

                        // `WaitStatus::Stopped(Pid, Signal)` は来ない。
                        // `man 2 waitpid` を確認すると、waitpid の引数(options)に `WUNTRACED` を渡した場合に、
                        // 子プロセスが停止した場合に返ってくると書いてある。つまり、`WaitStatus::Stopped` は `WUNTRACED` を
                        // 指定しない限りは返ってこない。
                        other => {
                            // 一旦他はまとめてエラーにしておくか。
                            eprintln!(
                                "[Error] 子プロセスが exit 以外の結果を示しました: {:?}",
                                other
                            );
                            exit(1);
                        }
                    }
                }

                // この分岐が子プロセス側の処理と思われる.
                //
                // この分岐は `fork` の直後なので、メモリを割りあげるなど、スレッド間の競合を防ぐロックを使うコードは書いてはダメ。
                // 基本的には、すぐに `exec` を実行するだけ。
                // なぜか？
                // -----
                // `fork` はプロセスをコピーするのだが、スレッドは実は、現在のスレッドしかコピーしない。
                // 仮に fork 元で、他のスレッドがロックを獲得していた場合、fork 後のプロセスでは、ロックを獲得しているプロセスが存在しないため、いつまでもロックが解放されずにデッドロックが起きる。
                // println! も、stdout のロックを取得するので、実行してはダメ。
                // -----
                Ok(ForkResult::Child) => {
                    // 子プロセスは read_fd を使わないので明示的に閉じておく。
                    // これをしないとどうなるか？
                    //
                    // ---------------------
                    // 親側で `drop(write_fd)` をしているのと逆向きの理由。
                    //
                    // 子プロセス側で、親プロセスが処理を終了した時に、もうこのパイプを読むプロセスが誰もいないことを表明しないといけない。
                    // でないと、子プロセス側は誰も読まない fd に書き込み続けることになってしまう。
                    // ---------------------
                    drop(read_fd);

                    // 子プロセスの stdout を write_fd にリダイレクトする。
                    // これによって、子プロセス側で標準出力に書きこんだデータが、パイプ経由で親プロセスから read_fd で読み込める。
                    dup2_stdout(write_fd).expect("[Error] 子プロセスでの dup に失敗しました。");

                    // 子のさらに子（親から見れば孫）など、さらに下の階層のプロセスもまとめて kill するためにプロセスグループを定義する。
                    // pid=0 は呼び出したプロセス自身を意味する。つまりここでは子プロセス。
                    if setpgid(Pid::from_raw(0), Pid::from_raw(0)).is_err() {
                        // `man timeout` に合わせて、コマンド自体が壊れていることを表す 125 にしておく。深い意図はない。
                        unsafe { _exit(125) };
                    };

                    // execvp は PATH を解釈してコマンドを実行してくれる.
                    // exec は Err(Errno) しか返さない。Ok を返さない。Infalliable は variant を持たないし、実装を見ても、Ok を返さないことはすぐわかった。
                    // なぜ Ok を返さない実装になっているのか？
                    // -> exec が成功した場合、戻ってくることが原理的にありえないから。
                    //
                    // -----
                    // execが成功した瞬間、プロセスの中身が別のプログラムに置き換わるので、「execvpの次の行」というもの自体が存在しない。
                    // なので、`Infallible` という値のない型を置くことで、起こりえないということを型で明示している。
                    // -----
                    //
                    //
                    // ===============================================================================
                    //
                    //
                    // Err をそのまま返していいのだろうか？ (`let _ = execvp(&cmd, &args)?;` といった感じで)
                    // →だめ!!!なぜか？
                    // ------
                    // fork した時に、子プロセスは親プロセスを丸ごとコピーする。メモリもコピーする。
                    //
                    // 前提知識：print! は毎回 syscall (write(2)) を読んでいるわけではなく、プログラムのメモリにバッファリングしておいて、ある程度溜まったタイミングで write(2) を読んでいる。
                    // そのある程度のタイミングがいくつかあるが、１つが「プログラムが終了した時」である。
                    //
                    // 親プロセスのバッファーに、まだ write(2) されていないデータがあると、fork した時に子プロセス側のバッファーにコピーされる。
                    // もし子プロセスが失敗して、通常の終了、つまり flush が実行されてしまうと、子プロセスの終了時にまずバッファーが出力される。
                    // そして親プロセスに処理がもどり、親プロセスが終了するときに、また同じバッファーのデータが出力されてしまう！
                    //
                    // そのため、子プロセスが失敗した時は、後始末をせずに即座に終了する必要がある。
                    //
                    // `man 3 exit` と `man 2 _exit` を確認した。前者は後始末をするが、後者は"即座に"終了する。ので、後者を実行すべき。
                    // ------
                    //
                    // なお、Errno とシェルの終了コードは全く別物と注意しておく。
                    // - Errno: システムコールが失敗した理由。これはカーネルが発行する。
                    // - 終了コード: プロセスが死んだ理由を親プロセスに伝える番号。これはカーネルは関係なく、シェル業界の慣習で番号が決められている。
                    match execvp(&cmd, &args) {
                        // 終了コードを以下のようにこのコマンドでは振り分けるようにします、というだけ。
                        Err(Errno::ENOENT) | Err(Errno::ENOTDIR) => unsafe { _exit(127) }, // 127 = コマンドが見つからなかった
                        _ => unsafe { _exit(126) }, // 126 = コマンドが見つかったが実行できなかった
                    };
                }

                Err(err) => {
                    eprintln!("[Error] fork に失敗しました: {err}");
                    exit(1);
                }
            }
        }
        None => {
            eprintln!("[Error] 引数が足りません。");
            exit(1);
        }
    }
}

fn get_timeout() -> Option<u32> {
    match std::env::var("MYRUN_TIMEOUT") {
        Ok(v) => Some(
            v.parse::<u32>()
                .expect("[Error] MYRUN_TIMEOUT には秒数を指定してください。"),
        ),
        Err(VarError::NotPresent) => None,
        Err(VarError::NotUnicode(_)) => {
            panic!("[Error] ここは到達しないはず。MYRUN_TIMEOUT は Unicode なので。")
        }
    }
}

enum ReadResult {
    // 最後まで読み取りが成功。読み取ったバイト数を返す。
    Success(usize),

    // タイムアウトのために処理が中断された場合。
    Timeout,
}

/// 親プロセスが SIGALARM を受け取ったことに由来する EINTR が返ってきた時は、直ちに処理を中断する（子プロセスのタイムアウトのため）。
/// それ以外の場合は単に再実行する。
fn read(fd: &OwnedFd, buf: &mut [u8]) -> nix::Result<ReadResult> {
    loop {
        match nix::unistd::read(fd, buf) {
            Err(Errno::EINTR) => {
                if IS_SIGALARM_TRIGGERED.load(Ordering::Relaxed) {
                    return Ok(ReadResult::Timeout);
                }
                continue;
            }
            Err(e) => return Err(e),
            Ok(n) => return Ok(ReadResult::Success(n)),
        }
    }
}
