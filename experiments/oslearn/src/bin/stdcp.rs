fn main() -> nix::Result<()> {
    let mut args = std::env::args().skip(1); // １つ目はコマンド自体なので飛ばす。
    match (args.next(), args.next()) {
        (Some(src), Some(dst)) => match std::fs::copy(src, dst) {
            Ok(_) => Ok(()),
            Err(err) => {
                eprintln!("[Error] ファイルコピーに失敗しました。{err}");
                std::process::exit(1);
            }
        },
        _ => {
            // エラーメッセージの詳細化は興味ないのでやらない。
            eprintln!("[Error] 引数不足");
            std::process::exit(1);
        }
    }
}
