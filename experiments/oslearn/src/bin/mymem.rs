use nix::sys::mman::{MapFlags, ProtFlags, madvise, mmap_anonymous};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::ForkResult::{Child, Parent};
use nix::unistd::fork;
use std::num::NonZeroUsize;
use std::process::exit;
use std::ptr;

/// 自分自身のメモリ地図を人間が読める形で表示するコマンド
///
/// /proc/self/maps は以下のようなデータが1行ずつ並んでいる。
///
/// 76620e828000-76620e9b0000 r-xp 00028000 08:01 6456   /usr/lib/x86_64-linux-gnu/libc.so.6
///
/// └──────── ① ────────────┘ └②┘ └── ③ ─┘ └④┘ └⑤┘   └────────── ⑥ ──────────┘
///
/// | 列 | 意味 |
/// | --- | --- |
/// | ① アドレス範囲 | 16進の `開始-終了`。**終了は含まない**。引き算するとサイズ(上の例は `0x188000` = 1568 KiB) |
/// | ② 権限 | `r`/`w`/`x` と、4文字目の `p`(private) か `s`(shared)。`-` は無し |
/// | ③ オフセット | **ファイルのどこからマップしているか**。この行は libc の 0x28000 バイト目以降 |
/// | ④ デバイス | そのファイルが載っているブロックデバイスの major:minor。`00:00` はファイル無し |
/// | ⑤ inode | ファイルの識別番号。**`0` はファイルに紐づいていない = 匿名マッピング** |
/// | ⑥ パス | ファイル名、`[heap]` などの特殊領域、または**空欄**(匿名)
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("step2") => run_step2(),
        Some("step3") => run_step3(),
        other => {
            println!(
                "[Fallback] 引数で明示された step が無効({other:?})なため、step2 を実行します。"
            );
            run_step2();
        }
    }
}

/// Step2 での実装
///
/// maps を確認し、mmap での確保や読み書きの挙動を確認した。
fn run_step2() {
    let mappings = read_mappings();
    print_total_mapping_size(&mappings);
    print_vmsize_vmrss(None);
    print_mappings(&mappings);

    println!("----- mmap 開始 -----");

    // Step2: 1 GiB を確保だけする。触らない。
    // Step3: 実際に書き込んでみる。
    unsafe {
        let addr = mmap_anonymous(
            None,
            NonZeroUsize::new(1 << 30).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();
        println!("mmap_anonymous の返した値: {:?}", addr);

        // 確保してまだ書き込んでいない時の VmRSS を確認する。
        print_vmsize_vmrss(None);

        // 1ページずつ書き込んでみる。
        let page_count = (1 << 30) / 4096; // 1 GiB / 4 KiB
        let base: *mut u8 = addr.as_ptr().cast(); // `c_void` は型がわからないメモリを表すので、変換が必要。
        for i in 0..page_count {
            // 1ページずつ書き込んでみる。以下は、u8 単位、つまり1Bだけ書き込んでる。
            ptr::write(base.add(i * 4096), 1);

            // 1ページずつ読み込んでみる方も見てみると、「ゼロページ」が使われるケースをみれるので面白い。
            // ptr::read(base.add(i * 4096));

            // 定期的に VmRSS を確認する。
            if i + 1 == 100 || i + 1 == 1000 || i + 1 == 10000 {
                println!("----- {} ページ書き込み完了 -----", i + 1);
                print_vmsize_vmrss(None);
            }

            if i + 1 == 10000 {
                println!("---- 物理メモリを返却する ----");
                madvise(
                    addr,
                    4096 * (i + 1), // ここまで書き込んだアドレス範囲
                    nix::sys::mman::MmapAdvise::MADV_DONTNEED,
                )
                .unwrap();

                // 物理メモリを返却したので、VmRSS が最初の値と一致するはず。
                // VmSize は予約したサイズなので、それは変わらないはず。
                print_vmsize_vmrss(None);

                // 返却した範囲をもう意図度読み込んだらどうなる？→予約はしたままなので、ゼロページが読み込まれるのでは？
                let result = ptr::read(base.add(i * 4096));
                println!("返却した 10000 ページ目を読み込んだ結果: {result}");
            }
        }
    }

    println!("----- mmap 完了 -----");

    let mappings = read_mappings();
    print_total_mapping_size(&mappings);
    print_vmsize_vmrss(None);
    print_mappings(&mappings);
}

fn run_step3() {
    unsafe {
        // 1GiB を確保する。
        let addr = mmap_anonymous(
            None,
            NonZeroUsize::new(1 << 30).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();

        // 全ページに書き込む(ループ以外での書き方あるのかな？？？)
        let page_count = (1 << 30) / 4096;
        let base: *mut u8 = addr.as_ptr().cast();
        for i in 0..page_count {
            ptr::write(base.add(i * 4096), 1);
        }

        match fork() {
            Ok(Parent { child }) => {
                print_vmsize_vmrss(Some("[parent][waitpid前]".to_string()));
                print_smaps_rollup(Some("[parent][waitpid前]".to_string()));
                match waitpid(child, None).unwrap() {
                    WaitStatus::Exited(pid, status) => {
                        println!("[parent][waitpid後] pid={pid} exited with {status}");
                        print_vmsize_vmrss(Some("[parent][waitpid後]".to_string()));
                        print_smaps_rollup(Some("[parent][waitpid後]".to_string()));
                        exit(status);
                    }
                    _ => {
                        // exit 以外の分岐は今回は興味ないので雑に扱う。
                        eprintln!("[parent][waitpid後] 子プロセスが exit 以外の理由で落ちました。");
                        exit(128);
                    }
                }
            }
            Ok(Child) => {
                // 今回は println! しか使っていない。改行あり。
                // 改行ありの場合は、すぐ flush される。なので、exec なしだけど、気にせず stdout に出力していくことにする。
                print_vmsize_vmrss(Some("[child][書き込み前]".to_string()));
                print_smaps_rollup(Some("[child][書き込み前]".to_string()));

                // 親と 1GiB の物理メモリを共有しているはず。
                // 先頭1万ページに書き込みを実施してみる。
                for i in 0..10000 {
                    ptr::write(base.add(i * 4096), 1);
                }

                print_vmsize_vmrss(Some("[child][書き込み後]".to_string()));
                print_smaps_rollup(Some("[child][書き込み後]".to_string()));

                // std::process::exit にしておく。
                // 今回は問題ないが、Rust の stdout を flush してくれるのは std::process::exit なので。
                // nix::libc::exit は C の `exit` なので、Rustのstdoutをflushしない。
                exit(0);
            }
            Err(err) => {
                eprintln!("[Error] fork に失敗しました: {err}");
                exit(1);
            }
        }
    }
}

/// `/proc/[pid]/maps` から読み取れるメモリのマッピングに対応する構造体.
struct Mapping {
    start: u64,
    end: u64,
    permissions: String,
    path: Option<String>,
}

impl Mapping {
    fn parse(line: &str) -> Mapping {
        let mut line = line.split_whitespace();

        // メモリ範囲のパース
        let mut first_field = line.next().unwrap().split('-');
        let start = u64::from_str_radix(first_field.next().unwrap(), 16).unwrap();
        let end = u64::from_str_radix(first_field.next().unwrap(), 16).unwrap();

        // 権限のパース
        let permissions = line.next().unwrap().to_string();

        // オフセット、デバイス、inode を飛ばす
        line.next();
        line.next();
        line.next();

        // path のパース
        let path = line.next().map(|s| s.to_string());

        Mapping {
            start,
            end,
            permissions,
            path,
        }
    }

    fn size(&self) -> u64 {
        self.end - self.start
    }
}

/// `/proc/self/maps` を読んでパースする。
fn read_mappings() -> Vec<Mapping> {
    std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .split("\n")
        .filter(|line| !line.is_empty())
        .map(Mapping::parse)
        .collect()
}

fn print_total_mapping_size(mappings: &[Mapping]) {
    let total: u64 = mappings.iter().map(|m| m.size()).sum();

    // 注意: 1024バイト単位なら、`KiB` と書くのが今は正しい。
    //      が、昔からの慣習で、`/proc/*/status` に `kB` と書いてあるらしいので合わせた。
    println!("全マッピングのサイズ合計: {}kB", total / 1024);
}

/// `/proc/self/status` の VmSize(仮想サイズ) と VmRSS(物理に載っている分)だけ出力する.
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

/// `/proc/self/smaps_rollup` の以下の行だけ出力する:
///
/// - Shared_Clean
/// - Shared_Dirty
/// - Private_Clean
/// - Private_Dirty
fn print_smaps_rollup(prefix: Option<String>) {
    let prefix = prefix.unwrap_or("".to_string());
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .unwrap()
        .split("\n")
        .filter(|line| !line.is_empty())
        .for_each(|line| {
            if line.starts_with("Shared_Clean:")
                || line.starts_with("Shared_Dirty:")
                || line.starts_with("Private_Clean:")
                || line.starts_with("Private_Dirty:")
                || line.starts_with("Pss:")
            {
                println!("{prefix}{line}");
            }
        })
}

fn print_mappings(mappings: &[Mapping]) {
    for m in mappings {
        println!(
            "{:x}-{:x} {:>8} KiB {} {}",
            m.start,
            m.end,
            m.size() / 1024,
            m.permissions,
            m.path.as_deref().unwrap_or("[匿名]")
        );
    }
}
