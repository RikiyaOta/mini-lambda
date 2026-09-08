//! 演習: std::fs::copy を使わずに、システムコールだけでファイルをコピーする。
//!
//! 使い方: mycp <src> <dst> <buf_size(option)>

use std::os::fd::OwnedFd;

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use nix::unistd;

fn main() -> nix::Result<()> {
    let mut args = std::env::args().skip(1); // １つ目はコマンド自体なので飛ばす。
    match (args.next(), args.next(), args.next()) {
        (Some(src), Some(dst), Some(buf_size)) => match buf_size.parse() {
            Ok(buf_size) => {
                if buf_size > 0 {
                    do_cp(&src, &dst, buf_size)
                } else {
                    eprintln!("[Error] 第３引数のバッファサイズは正の整数を入力してください.");
                    std::process::exit(1);
                }
            }
            Err(_e) => {
                eprintln!("[Error] 第３引数にはバッファサイズ（bytes）を整数値で入力してください.");
                std::process::exit(1);
            }
        },
        (Some(src), Some(dst), None) => do_cp(&src, &dst, 8192),
        _ => {
            // エラーメッセージの詳細化は興味ないのでやらない。
            eprintln!("[Error] 引数不足");
            std::process::exit(1);
        }
    }
}

fn do_cp(src: &str, dst: &str, buf_size: usize) -> nix::Result<()> {
    let src_fd = open(src, OFlag::O_RDONLY, Mode::empty())?;
    let dst_fd = open(
        dst,
        OFlag::O_WRONLY | OFlag::O_TRUNC | OFlag::O_CREAT,
        Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH, // permission 644 で作る.
    )?;
    let mut buf: Box<[u8]> = vec![0u8; buf_size].into_boxed_slice(); // サイズが実行時にしか決まらないため、ヒープにバッファを確保する。
    loop {
        let n = read(&src_fd, &mut buf)?;
        if n == 0 {
            // 読み込みが完了していて、何も読むものがなかった。
            // つまり、コピー完了！
            break;
        } else {
            // いくらか buf に読み取ったデータがある。
            // dst に書き込みが必要。
            write_all(&dst_fd, &buf[..n])?;
        }
    }

    Ok(())

    // ここでスコープ抜けるから、close は明示的に呼ばなくても良い。
    // std::os::fd::OwnedFd が面倒を見てくれる。素敵。
}

fn read(fd: &OwnedFd, buf: &mut [u8]) -> nix::Result<usize> {
    loop {
        match unistd::read(fd, buf) {
            Err(Errno::EINTR) => continue,
            other => return other,
        }
    }
}

/// write は n バイト書き込むつもりでも、それより少ないバイトしか書き込まない場合がある。
/// n バイト全て書き込むようにループさせる。
/// std::io::Write::write_all がやっていることらしい。
///
/// ## NOTE
///
/// Rust は末尾再帰を保証しないらしいので、再帰ではなくループで書いている。
fn write_all(fd: &OwnedFd, buf: &[u8]) -> nix::Result<()> {
    let n = buf.len();
    let mut m = 0;
    loop {
        match unistd::write(fd, &buf[m..]) {
            // プロセスがブロックしている最中にシグナルが届くと、カーネルがシステムコールを途中で打ち切ってしまうらしい。
            // ただし、EINTR エラーの場合は1バイトも書き込んではいない。なので、単に再実行で良い。
            // この手のエラーを再実行する仕組みがカーネルにある(`SA_RESTART`)が、再開されない syscall もあるので、結局自分で捌いた方が安全らしい。
            Err(Errno::EINTR) => continue,

            // それ以外のエラーは普通にエラーとして返す。
            Err(err) => return Err(err),

            Ok(0) => {
                panic!(
                    "0を返すことは想定されない。 `man 2 write` にも明記されていない。が、行儀の悪いデバイスドライバなどで発生した場合に、無限ループになるので明示的に落とす。std::io::Write::write_allでは独自のエラーを返している。"
                );
            }

            Ok(written) => {
                m += written;
                if n == m {
                    // 依頼した長さと実際に書き込んだ長さが一致。これは正常終了.
                    return Ok(());
                }
                // 長さが違うと言うことは、全部は書き込んでくれていないはず。
                // buf.len() - m だけ残っている。&buf[..m] までが書き込まれた。
                // &buf[m..] からもう一度書いてくれればいい。
            }
        }
    }
}
