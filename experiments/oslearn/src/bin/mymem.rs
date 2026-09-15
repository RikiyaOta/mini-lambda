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
    print_total_mapping_size();
    print_vmsize_vmrss();
}

fn print_total_mapping_size() {
    let total: u64 = std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .split("\n")
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut first_field = line.split_whitespace().next().unwrap().split('-');
            let left = first_field.next().unwrap(); // アドレス範囲の左
            let right = first_field.next().unwrap(); // アドレス範囲の右

            let left = u64::from_str_radix(left, 16).unwrap();
            let right = u64::from_str_radix(right, 16).unwrap();

            right - left
        })
        .sum();

    // 注意: 1024バイト単位なら、`KiB` と書くのが今は正しい。
    //      が、昔からの慣習で、`/proc/*/status` に `kB` と書いてあるらしいので合わせた。
    println!("全マッピングのサイズ合計={}kB", total / 1024);
}

/// `/proc/self/status` の VmSize(仮想サイズ) と VmRSS(物理に載っている分)だけ出力する.
fn print_vmsize_vmrss() {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .split("\n")
        .filter(|line| !line.is_empty())
        .for_each(|line| {
            if line.starts_with("VmSize:") || line.starts_with("VmRSS:") {
                println!("{line}");
            }
        });
}
