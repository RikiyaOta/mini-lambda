# Phase -1: 仮想メモリ (mymem)

**日付**: 2026-09-15 開始
**成果物**: `experiments/oslearn/src/bin/mymem.rs`

`mycp`(システムコールとfd)、`myrun`(プロセス/fork/exec/シグナル/パイプ)に続く3本目、
Phase -1 最後の演習。

`myrun` の最後にたどり着いた「`fork` が重いのはアドレス空間のコピーだから」
「だから `std::process::Command` は `clone3(CLONE_VM|CLONE_VFORK)` を使う」を、
ここで**実際に測って確かめる**。そして Phase 3 の EPT(Extended Page Table)は
「この変換がもう1段増えるだけ」という状態にして次へ進む。

---

## 演習の全体設計(全5ステップ)

| Step | やること                                     | 押さえるもの                                        | 状態 |
| ---- | -------------------------------------------- | --------------------------------------------------- | ---- |
| 1    | 自分のアドレス空間を印字する                 | 仮想アドレス空間の構造、VSZ と RSS の差             | 🔶 進行中(09-15) |
| 2    | `mmap` で 1GiB 確保して1ページずつ触る       | **確保と割り当ては別物**、ページフォルト、遅延割り当て | -    |
| 3    | 大きなメモリを持ったまま `fork` する         | **CoW (Copy-on-Write)**、`fork` が本当にコピーするもの | -    |
| 4    | 仮想アドレス → 物理アドレスを自分で引く      | **ページテーブル**、共有の実証 ★EPT 直結            | -    |
| 5    | ページフォルトを自分のコードで処理する       | `userfaultfd` ★Phase 6 のスナップショット直結       | -    |

---

## 環境

`mycp` / `myrun` と同じ GCP `vmm-dev` 上。`experiments/oslearn` に bin を足す。

`Cargo.toml` の `nix` の feature に `mman`(Step 2 の `mmap`)と `resource`(`getrusage`)を追加した。

### 動作確認コマンド

```bash
cd experiments/oslearn
cargo run --bin mymem
```

---

## Step 1: 自分のアドレス空間を印字する

**作るもの**: 自分自身のメモリ地図を、人間が読める形で表示するコマンド。

### 読むもの

`man 5 proc` の `/proc/[pid]/maps` と `/proc/[pid]/status` の項。

```
/proc/self/maps          1行1マッピング:
                         アドレス範囲 / 権限 / オフセット / デバイス / inode / パス
/proc/self/status        VmSize: (仮想サイズ) と VmRSS: (物理に載っている分) の行がある
/proc/self/smaps_rollup  上を集計済みにしたもの(答え合わせ用)
```

### やること

1. `/proc/self/maps` を読んで、各行を **サイズ付き** で表示する
   (アドレス範囲は16進なので引き算する)
2. 全マッピングのサイズ合計と、`/proc/self/status` の `VmSize:` を突き合わせる
3. `VmRSS:` も表示して、**仮想と物理の比**を出す

新しい API はほぼ不要。`std::fs::read_to_string` と `u64::from_str_radix(s, 16)` で足りる。

### 予告する罠(自分で確かめる)

- [x] **測る前に予想する。** VSZ と VmRSS の比は何倍になると思うか
      → 予想できなかった。それでよかった。**比率に決まった値は無い**ことが分かったのが収穫(下記)
- [ ] `maps` を読む自分のコード自身が、マッピングを増やしている。どこで増えるか
- [ ] **罠A**: `[heap]` が見当たらないかもしれない。無いとしたら、なぜか
      → 調べ方: `strace -e trace=brk,mmap ./target/debug/mymem`。
        `[heap]` を作る syscall と、匿名メモリを作る syscall は別物。どちらが呼ばれているか
- [ ] **罠B**: 権限が `---p` の領域がある。読めも書けも実行もできないメモリを、なぜ予約しているのか
      → 調べ方: 全行出力で `---p` の行を探し、**その前後の行**とサイズを見る
- [ ] **罠C**: 同じバイナリを2回動かして出力を `diff` する。何かが毎回変わる。それは何で、なぜか
      → 調べ方: `./mymem > /tmp/a.txt; ./mymem > /tmp/b.txt; diff /tmp/a.txt /tmp/b.txt`。
        **変わるもの**と**変わらないもの**を分ける

### 現在地 (2026-09-15 時点)

- できたこと: `/proc/self/maps` を読んでサイズ**合計**を出し、`VmSize:` / `VmRSS:` と並べて表示した
- **残り**: 手順1の「**各行をサイズ付きで表示する**」がまだ。罠A〜Cは全行が出ないと解けないので、ここが次の一手

### 使った道具: マッピングごとの実物を見る

`maps` は予約表だけだが、`/proc/self/smaps` は各マッピングに `Size:`(予約)と `Rss:`(実物)が付く。

```bash
awk '/^[0-9a-f]+-/ {name=$6; if(name=="")name="[匿名]"; perm=$2}
     /^Size:/ {size=$2}
     /^Rss:/  {rss=$2; printf "%8d KiB %8d KiB  %s %s\n", size, rss, perm, name}' /proc/self/smaps | sort -rn
```

実行例(gawk 自身):

```
   Size      Rss
  2988 KiB    64 KiB  r--p /usr/lib/locale/locale-archive
  1568 KiB  1120 KiB  r-xp /usr/lib/.../libc.so.6
   352 KiB     0 KiB  r--p /usr/lib/.../libm.so.6      ← 予約だけ。物理メモリ ゼロ
   172 KiB   172 KiB  r-xp /usr/lib/.../ld-linux-x86-64.so.2
   132 KiB    32 KiB  rw-p [stack]
```

### 分かったこと

> 以下は Step 1 で確定した事実。**自分の言葉で書き直す**。

- [ ] `VSZ`(`VmSize:`)= 予約、`RSS`(`VmRSS:`)= 実物。それぞれ `/proc/self/status` のどの行か。
      RSS の R は Resident(常駐している)
- [ ] **`maps` の全サイズ合計 == `VmSize` + `[vsyscall]` の 4 KiB**。1バイトも狂わずに合う
      (`[vsyscall]` だけ `VmSize` に数えられない)
- [ ] 表示は `kB` だが**中身は KiB(1024)**。正しくは KiB(kibibyte, IEC 1998)で、カーネルのラベルが不正確。
      `ps` / `free` / `top` も同じ。メモリが 1024 単位なのは、ページサイズもアドレス空間も 2 の冪だから。
      ディスク容量が 10^3 なのはその制約が無いため(「1TB の SSD が 931GB」の正体)
- [ ] VSZ と RSS の差の正体は **「貼ったが一度も触っていないページ」**。
      証拠は `libm.so.6` の `r--p` が `Size 352 KiB / Rss 0 KiB`
- [ ] **比率に決まった値は無い**。マップした領域のうち何割を実際に触ったかで決まる
      (実測: `mymem` は約1.5倍、`grep` は約167倍)
- [ ] `smaps` は `maps` に `Size:` / `Rss:` を足したもの

---

## 次にやること

- [ ] Step 1: 自分のアドレス空間を印字する 🔶 進行中
      - [x] `maps` のサイズ合計を出し、`VmSize:` / `VmRSS:` と並べる
      - [ ] **各行をサイズ付きで表示する**(ここから罠A〜Cが解ける)
      - [ ] 罠A(`[heap]`) / 罠B(`---p`) / 罠C(2回動かすと変わるもの)
      - [ ] 「分かったこと」を自分の言葉で書く
- [ ] Step 2: `mmap` で 1GiB 確保して1ページずつ触る
- [ ] Step 3: 大きなメモリを持ったまま `fork` する
- [ ] Step 4: 仮想アドレス → 物理アドレスを自分で引く
- [ ] Step 5: ページフォルトを自分のコードで処理する
- [ ] `docs/glossary.md` に追加する: VSZ / RSS / KiB と kB / ページ / 遅延割り当て
      (演習が進んだら CoW / ページテーブル / `userfaultfd` も)

---

## 自分の言葉で書く

> Step 5 を終えてから埋める。**書けないなら理解できていない。**

### なぜ「仮想」アドレスという間接層があるのか

<!-- ここに書く -->

### ページテーブルと EPT は何が同じで何が違うか

<!-- ここに書く -->
