use nix::errno::Errno;
use nix::libc::_exit;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, execvp, fork};
use std::ffi::CString;
use std::process::exit;

fn main() -> nix::Result<()> {
    let mut args = std::env::args().skip(1);

    match args.next() {
        Some(cmd) => {
            match unsafe { fork() } {
                // この分岐が親プロセス側の処理と思われる.
                Ok(ForkResult::Parent { child }) => {
                    // 子プロセスの処理が完了するのを待つ。
                    match waitpid(child, None).expect("waitpid が失敗しました。") {
                        WaitStatus::Exited(pid, status) => {
                            println!("[myrun] pid={pid} exited with {status}");
                            // 一旦 Step1 では 0 で固定する(Step3 で子に合わせる)
                            exit(0);
                        }
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
                Ok(ForkResult::Child) => {
                    // Rust の文字列(&str)は、ポインタであり、文字の長さも持っている。
                    // システムコールのインターフェースはCの規約で決まっている。Cでは、文字列はただのポインタで、長さは持っていない( \0 (NULバイト) が終端というルール)
                    // なので、Rust の &str をそのままでは渡せないので、型できちんと区別されている。
                    let cmd = CString::new(cmd).expect("cmd を CString に変換できませんでした。");

                    // 引数を CString に変換する。
                    let mut args: Vec<_> = args
                        .map(|s| CString::new(s).expect("引数を String に変換できませんでした。"))
                        .collect();

                    // 引数の先頭はコマンド自身を指定する必要がある。
                    args.insert(0, cmd.clone());

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
