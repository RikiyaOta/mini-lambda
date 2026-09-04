//! 演習: std::fs::copy を使わずに、システムコールだけでファイルをコピーする。
//!
//! 使い方: mycp <src> <dst>

use std::os::fd::OwnedFd;

use nix::errno::Errno;
use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use nix::unistd;

fn main() -> nix::Result<()> {
    let mut args = std::env::args().skip(1); // １つ目はコマンド自体なので飛ばす。
    match (args.next(), args.next()) {
        (Some(src), Some(dst)) => {
            let src_fd = open(src.as_str(), OFlag::O_RDONLY, Mode::empty())?;
            let dst_fd = open(
                dst.as_str(),
                OFlag::O_WRONLY | OFlag::O_TRUNC | OFlag::O_CREAT,
                Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH, // permission 644 で作る.
            )?;
            let mut buf = [0u8; 8192];
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
        _ => {
            // エラーメッセージの詳細化は興味ないのでやらない。
            eprintln!("[Error] 引数不足");
            std::process::exit(1);
        }
    }
}

fn read(fd: &OwnedFd, buf: &mut [u8]) -> nix::Result<usize> {
    match unistd::read(fd, buf) {
        Err(Errno::EINTR) => read(fd, buf),
        other => other,
    }
}

// write は n バイト書き込むつもりでも、それより少ないバイトしか書き込まない場合がある。
// n バイト全て書き込むようにループさせる。
// std::io::Write::write_all がやっていることらしい。
fn write_all(fd: &OwnedFd, buf: &[u8]) -> nix::Result<()> {
    match unistd::write(fd, buf) {
        Err(Errno::EINTR) => write_all(fd, buf),
        Err(err) => Err(err),
        Ok(m) => {
            if m == buf.len() {
                // 依頼した長さと実際に書き込んだ長さが一致。これは正常終了.
                Ok(())
            } else {
                // 長さが違うと言うことは、全部は書き込んでくれていないはず。
                // buf.len() - m だけ残っている。&buf[..m] までが書き込まれた。
                // &buf[m..] からもう一度書いてくれればいい。再起呼び出しがシンプル（ループの方が性能いいのかな？）
                write_all(fd, &buf[m..])
            }
        }
    }
}
