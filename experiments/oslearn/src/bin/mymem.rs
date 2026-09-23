use nix::sys::mman::{MapFlags, ProtFlags, madvise, mmap_anonymous};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::ForkResult::{Child, Parent};
use nix::unistd::fork;
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroUsize;
use std::process::exit;
use std::ptr;
use userfaultfd::{Event, UffdBuilder};

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
        Some("step4") => run_step4(),
        Some("step5-1") => run_step5_1(),
        Some("step5-2") => run_step5_2(),
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

fn run_step4() {
    unsafe {
        // mmap しておき、書き込んで私有ページにしておく.
        let addr = mmap_anonymous(
            None,
            NonZeroUsize::new(1 << 30).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();
        let base: *mut u8 = addr.as_ptr().cast();
        ptr::write(base, 1);
        let entry = pagemap_entry(addr.as_ptr() as usize);
        println!("fork 前: {:?}", entry);

        match fork() {
            Ok(Parent { child }) => {
                let entry = pagemap_entry(addr.as_ptr() as usize);
                println!("[親] waitpid 前: {:?}", entry);

                match waitpid(child, None).unwrap() {
                    WaitStatus::Exited(_pid, _status) => {
                        let entry = pagemap_entry(addr.as_ptr() as usize);
                        println!("[親] waitpid 後(子がexitした後): {:?}", entry);
                        exit(0);
                    }
                    _ => {
                        // exit 以外の分岐は今回は興味ないので雑に扱う。
                        eprintln!("[parent][waitpid後] 子プロセスが exit 以外の理由で落ちました。");
                        exit(128);
                    }
                }
            }
            Ok(Child) => {
                let entry = pagemap_entry(addr.as_ptr() as usize);
                println!("[子] 書き込み前: {:?}", entry);

                // 子で書き込んでみる.
                ptr::write(base, 1);

                // 書き込んだ後にどうなるか？
                let entry = pagemap_entry(addr.as_ptr() as usize);
                println!("[子] 書き込み後: {:?}", entry);

                exit(0);
            }
            Err(err) => {
                eprintln!("[Error] fork に失敗しました: {err}");
                exit(1);
            }
        }
    }
}

fn run_step5_1() {
    unsafe {
        // 4ページほど適当に mmap して予約しておく。
        // ※4ページ = 4KiB * 4 = 16KiB = 2^14 B
        let length = 1 << 14;
        let start_addr = mmap_anonymous(
            None,
            NonZeroUsize::new(length).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();

        println!("[main thread] start_addr={:?}", start_addr);

        // `NonNull<c_void>` は `Send` を実装していないので、スレッドを跨げない。
        // なので `usize` に変換しておく。
        let start = start_addr.as_ptr() as usize;

        let uffd = UffdBuilder::new().user_mode_only(true).create().unwrap();
        let _ioctl_flag = uffd.register(start_addr.as_ptr(), length).unwrap();

        std::thread::spawn(move || {
            loop {
                match uffd.read_event().unwrap() {
                    Some(event) => match event {
                        Event::Pagefault { kind, rw, addr } => {
                            // 自分で確保した4ページのうち、どのページなのかを算出する。
                            // そのために、最初に確保した時のアドレスとの差からページ番号(1,2,3,4)を算出する。
                            // 注意：イベントで渡される addr はページ境界とは限らない！
                            let diff = addr as usize - start;
                            let page_num = (diff / 4096) + 1;
                            println!(
                                "[another thread] ページフォルト発生(page_num={}): {:?}, {:?}, {:?}",
                                page_num, kind, rw, addr
                            );

                            // そのページ番号でうめた4096バイト(4KiB)を `copy` で流し込む.
                            //
                            // write だと、このスレッド(ハンドラ)自身がページフォルトを起こす。
                            // カーネルはハンドラスレッドを眠らせて uffd にイベント送るが、
                            // ハンドラが眠っているので応答する人がいないため、永遠に待つことになる。
                            // なので以下のコードだとダメ。止まってしまうことまで確認した。
                            //
                            // ```
                            // ptr::write(base, [page_num as u8; 4096]);
                            // ```
                            //
                            // 代わりに `copy` を使う
                            let buf = [page_num as u8; 4096];
                            let src = buf.as_ptr() as *const c_void;
                            let dst = (start + (page_num - 1) * 4096) as *mut c_void;
                            uffd.copy(src, dst, 4096, true).unwrap();
                        }
                        other => {
                            println!(
                                "[another thread] ページフォルト以外のイベント検知: {:?}",
                                other
                            );
                        }
                    },
                    None => {
                        // non_block = true の時、まだ読み込みがまだできない時は None になるらしい。
                        // Non Block がなにを意味しているかわかっていない。。。
                        // 今回は、デフォルトの non_block = false で実行しているので、特になにもしない。
                        // ログだけ出しておこう。念の為。
                        println!("[anather thread][WARN] uffd.read_event() で None が返りました。");
                    }
                }
            }
        });

        // メインスレッドでは、各ページの先頭1Bを読む（書かない）
        // 読んだ値を表示する。
        let base: *mut u8 = start_addr.as_ptr().cast();
        for i in 0..=3 {
            let result = ptr::read(base.add(i * 4096));
            println!("[main thread] result={result}");

            // 同じページを2回読んで実験してみる。
            // すでに上の read でページフォルトを起こしたので、この下の read では
            // ページフォルトは発生しないと予想できる。
            let result = ptr::read(base.add(i * 4096));
            println!("[main thread] 2回目 result={result}");
        }
    }
}

fn run_step5_2() {
    unsafe {
        // 200ページ = 4KiB * 200 = 819200 バイトを予約して register する（step5-1と同じ）.
        let length = (1 << 10) * 4 * 200;
        let start_addr = mmap_anonymous(
            None,
            NonZeroUsize::new(length).unwrap(),
            ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
            MapFlags::MAP_PRIVATE,
        )
        .unwrap();

        print_vmsize_vmrss(Some("[main thread] ".to_string()));
        println!("[main thread] start_addr={:?}", start_addr);

        let start = start_addr.as_ptr() as usize;

        let uffd = UffdBuilder::new().user_mode_only(true).create().unwrap();
        let _ioctl_flag = uffd.register(start_addr.as_ptr(), length).unwrap();

        // ハンドラスレッドでは、スナップショットファイルを開いておき、
        // フォルトが来たら、ページ番号のファイルのオフセットを計算し、そこから4096B読んで、
        // それを `copy` の src にする。読み込んだページ数を数えて、その都度表示する。
        std::thread::spawn(move || {
            let mut read_page_count = 0;
            let mut f = File::open("/tmp/snapshot.bin").unwrap();
            loop {
                match uffd.read_event().unwrap() {
                    Some(Event::Pagefault { kind, rw, addr }) => {
                        read_page_count += 1;
                        let i = (addr as usize - start) / 4096;

                        let mut buf = [0u8; 4096];
                        f.seek(SeekFrom::Start((i * 4096) as u64)).unwrap(); // ここはメモリの位置でなく、ファイルの位置であることに注意。
                        f.read_exact(&mut buf).unwrap();

                        let dst = (start + i * 4096) as *mut c_void;

                        uffd.copy(buf.as_ptr() as *const c_void, dst, 4096, true)
                            .unwrap();

                        println!(
                            "[Handler Thread] Pagefault! (i={i})(読み込んだページ数={read_page_count}): {:?}, {:?}, {:?}",
                            kind, rw, addr
                        );
                    }
                    other => {
                        println!(
                            "[Handler Thread] Pagefault 以外のイベントを検知しました: {:?}",
                            other
                        );
                    }
                }
            }
        });

        // メインスレッドでは一部のページだけを読む
        // ここでは、適当に、10ページおきに20ページだけ読むことにする。
        let base: *mut u8 = start_addr.as_ptr().cast();
        for i in 0..20 {
            let page_i = i * 10; // 0, 10, 20, 30, ...
            let page_num = page_i + 1; // 1, 11, 21, 31, ...

            // 一部のページで、先に書いてから読み直してみる。
            if i == 4 {
                ptr::write(base.add(page_i * 4096), 255);

                let first_byte = ptr::read(base.add(page_i * 4096));
                let second_byte = ptr::read(base.add(page_i * 4096 + 1));
                println!("[main thread] first_byte={first_byte}, second_byte={second_byte}");
            }

            let result = ptr::read(base.add(page_i * 4096));
            println!("[main thread] page_num={page_num}, result={result}");
        }

        print_vmsize_vmrss(Some("[main thread] ".to_string()));
    }
}

/// 指定された仮想アドレスが属するページに対応する `/proc/self/pagemap` の情報を返す。
///
/// pagemap の中身は以下のような、`u64`が仮想ページ番号順にぎっしり並んだものと思って良い。
/// `Vec<u64>` の添え字が仮想ページ番号に対応する感じ。
///
/// ```
/// [ページ0の情報][ページ1の情報][ページ2の情報][ページ3の情報] ...
/// ←─ 8 バイト ─→←─ 8 バイト ─→←─ 8 バイト ─→
/// バイト位置: 0            8            16           24
/// ```
fn pagemap_entry(virt_addr: usize) -> PagemapEntry {
    let mut f =
        File::open("/proc/self/pagemap").expect("/proc/self/pagemap の open に失敗しました。");
    let _ = f
        // virt_addr / 4096 --> 4096B = 4KiB はページの大きさなので、4096で割って、仮想ページ番号を算出している。
        // （16進数表示で 4096 = 0x1000 なので、virt_addr の16進数での末尾3桁を落としている）
        // あとは pagemap が巨大な u64 (=8B) の配列だと思えば、仮想ページ番号 * 8 をすれば、その位置までシークできる。
        .seek(SeekFrom::Start((virt_addr / 4096 * 8) as u64))
        .unwrap();

    let mut buf = [0u8; 8]; // 8Bだけ読み込む
    f.read_exact(&mut buf).unwrap();

    let entry = u64::from_le_bytes(buf);

    let present = (entry >> 63) & 1 == 1; // 63ビット目を取り出す
    let pfn = entry & ((1u64 << 55) - 1); // 下位55ビットを取り出す((1u64<<55)-1は2進数で1が55個並んだ数)

    PagemapEntry { present, pfn }
}

/// `/proc/self/pagemap` の各エントリー（8バイト）に詰め込まれている情報に対応する構造体
#[derive(Debug)]
struct PagemapEntry {
    /// 63ビット目。
    ///
    /// この仮想ページに今この瞬間、物理ページが結びついているか。
    ///
    /// - false: VMA(予約)はあるがまだ実物なし。
    /// - true: 実物がある。つまり、ページテーブル（CPUが見るやつ）に行がある。
    present: bool,

    /// 下位55ビット。
    ///
    /// 物理ページ番号。
    /// `pfn * 4096(=4KiB)` が物理アドレスになる。
    pfn: u64,
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
