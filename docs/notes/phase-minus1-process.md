# Phase -1: プロセスとfork/exec、シグナル、パイプ (myrun)

**日付**: 2026-09-09 開始
**成果物**: `experiments/oslearn/src/bin/myrun.rs`

`mycp` に続く2本目の演習。ROADMAP の Phase -1 に残っていた
「プロセスとfork/exec、シグナル、パイプ」を1本の演習で回収する。

作るものは **「子プロセスを起動し、出力を受け取り、時間切れなら殺す」コマンド**。
ROADMAP の Phase 1 に書いてある「`std::process::Command` でサブプロセス実行、
stdout をレスポンスに、タイムアウト処理」の中身そのものなので、
ここで手を動かしておくと Phase 1 が「知っているものを速く書く」作業になる。

---

## 演習の全体設計(全5ステップ)

| Step | やること                                                | 押さえるもの                | 状態                |
| ---- | ------------------------------------------------------- | --------------------------- | ------------------- |
| 1    | `fork` + `exec` + `waitpid` でコマンドを実行し終了ステータスを表示 | プロセス生成、PID、ゾンビ   | ✅ 完了(09-09)      |
| 2    | `pipe` で子の stdout を親が読み取る                     | fd の付け替え、EOF          | ✅ 完了(09-13)      |
| 3    | 終了ステータスを分解する(正常終了 / シグナルで死んだ)   | シグナル                    | ✅ 完了(09-14)      |
| 4    | 時間内に終わらなければ子を殺す                          | `kill`、SIGTERM と SIGKILL  | ⬜ **次はここから** |
| 5    | `strace -f` で観察する                                  | `clone`/`execve`/`pipe2`/`dup2`/`wait4` | ⬜       |

---

## 環境

`mycp` と同じ GCP `vmm-dev` 上。`experiments/oslearn` に bin を足していく形。

`Cargo.toml` の `nix` の feature に `process` と `signal` を追加した。

```toml
nix = { version = "0.31.3", features = ["fs", "process", "signal"] }
```

### 動作確認コマンド

```bash
cd experiments/oslearn
cargo build --bin myrun
./target/debug/myrun echo hello         # → hello / exited with 0
./target/debug/myrun sh -c 'exit 3'     # → exited with 3
./target/debug/myrun nosuchcmd          # → exited with 127
touch /tmp/notexec && chmod -x /tmp/notexec
./target/debug/myrun /tmp/notexec       # → exited with 126
./target/debug/myrun /tmp               # → exited with 126 (ディレクトリ)

# Step 2 以降: 親が出力を受け取る形になる
./target/debug/myrun echo hello         # → captured 6 bytes: "hello\n"
./target/debug/myrun ls -la /usr/bin    # → captured 72570 bytes (64KiB 超え。デッドロックの回帰テスト)

# Step 3 以降: myrun 自身の終了コードが子に追従する
./target/debug/myrun sh -c 'exit 3';            echo $?   # → exited with 3 / 3
./target/debug/myrun sh -c 'kill -SEGV $$';     echo $?   # → killed by signal SIGSEGV / 139 (=128+11)
./target/debug/myrun sh -c 'kill -TERM $$';     echo $?   # → killed by signal SIGTERM / 143 (=128+15)
./target/debug/myrun sh -c 'kill -KILL $$';     echo $?   # → killed by signal SIGKILL / 137 (=128+9)
```

---

## 分かったこと(Step 1)

### Unix には「プログラムを起動する」という単一の操作が無い

2段構えになっている。

1. **`fork`** — 今動いている自分自身を丸ごと複製する。子は**同じコードの、同じ場所から**動き出す
2. **`exec`** — 今の自分の中身を別のプログラムで**上書きする**。PID は変わらず、中身だけ入れ替わる

だから `ls` を動かすには「自分のコピーを作り、そのコピーに `ls` を上書きする」という手順を踏む。
`fork()` の直後に「今の自分は親か子か」を分岐で判定する形になる。

### `argv[0]` にはコマンド名自身を入れる ★最初に踏んだ罠

`execvp(path, args)` の `args` の**第1要素はコマンド名自身**。実際の引数は第2要素から。
`man 3 exec` に "The first argument, by convention, should point to the filename
associated with the file being executed." とある。

ここを間違えて引数を1つずつズラしていたときの症状:

| 実行 | 症状 |
| --- | --- |
| `ls -la /tmp` | 出力は出るが `-la` が消えている(**成功したように見えた**) |
| `echo hello` | 何も出ない(空行だけ出ていた) |
| `sh -c 'exit 3'` | よく分からないエラー |

`ls` が「それっぽく動いてしまった」のが厄介だった。`mycp` のときの `sudo cmp` と同じ形で、
**「それっぽい出力が出た」を「成功した」と読んでしまう**パターン。

実際に何が渡ったかは推測せず、strace で見るのが速い。

```bash
strace -f -e trace=execve ./target/debug/myrun echo hello
```

`-f` は「fork した子も追いかける」フラグ。子の中で `execve` するのでこれが無いと見えない。

### `CStr` / `CString` は「C言語のための型」ではなく「カーネルとの約束を守るための型」

Rust の `&str` は **ポインタ + 長さ**。C の文字列は **ポインタだけ**で、
終わりは `\0`(NUL)で示す(NUL終端)。

`execve(2)` のシグネチャは `int execve(const char *pathname, ...)` で、
**ポインタ1つしか渡せない**。長さを渡す口が無い。カーネルは渡されたアドレスから
`\0` が出るまで読む。なので `&str` の中身をそのまま渡すと、隣接メモリを読み続けてしまう。

システムコールのインターフェースは C の **ABI** で決まっているので、
Rust 側で型を分けて守っている。

> **ABI (Application Binary Interface)**: 引数をどのレジスタに置くか、構造体をメモリにどう並べるか、
> といった機械語レベルの取り決め。ソースレベルの約束が API、コンパイル後の約束が ABI。

`String` : `&str` = `CString` : `&CStr` の関係。`CString::new` が `Result` なのは、
途中に `\0` があると C の文字列として表現できないため。

### `exec` に失敗した子が普通に `main` を抜けると、出力が二重になる ★重要

`fork` はメモリを丸ごとコピーするので、**まだ書き出していない stdout のバッファもコピーされる**。

```
【親】 print!("start ")  → 親のバッファ = ["start "]  ← まだ画面に出ていない
       fork()            → 子のバッファにもコピーされる
   ┌────────────┴────────────┐
【親】                      【子】
                            execvp 失敗 → ? で main を抜ける
                            終了時の後始末でフラッシュ → "start " ★1回目
waitpid で回収
exit(0) → フラッシュ → "start " ★2回目
```

実測(`_exit` に直す前):

```
$ ./target/debug/myrun nosuchcmd
Error: ENOENT
start start [myrun] pid=25938 exited with 1     ← start が2回
```

`exec` が**成功**した場合は1回しか出ない。`exec` はプロセスの中身を丸ごと入れ替えるので、
子が持っていたバッファのコピーは**吐き出される前に消滅する**から。
つまり**失敗したときだけ壊れる**。成功時のテストでは気づけない。

対処は **`_exit(2)`**。`exit(3)` / `std::process::exit()` は終了時の後始末(バッファのフラッシュ)を
するが、`_exit` はしない。`std::process::exit()` でもフラッシュは起きることを実測で確認した。

#### バッファリングそのものについて

`print!` は毎回 `write(2)` を呼ばない。syscall は高い(Step 4 で 1回あたり約300ns と実測)ので、
メモリ上のバッファに溜めてからまとめて `write` する。`BufReader` の書き込み版。

フラッシュのきっかけは4つ: バッファ満杯 / 改行 / 明示的な `flush()` / **プログラム終了時**。

- **Rust の `stdout` は、出力先が端末でもファイルでも常に行バッファリング**(`LineWriter`)。
  「端末なら行バッファ、ファイルならブロックバッファ」と切り替わるのは **C の `stdio`** の挙動。
  man page はC前提で書かれているので注意
- **`stderr` はバッファリングされない。** だから `_exit` で即死しても `eprintln!` は出る

### `Errno` と 終了コード は全く別の番号体系 ★重要

| | errno | 終了コード (exit status) |
| --- | --- | --- |
| 何を伝えるか | システムコールが失敗した理由 | プロセスがどういう結末で終わったか |
| 誰から誰へ | カーネル → syscall を呼んだプログラム自身 | 死ぬプロセス → 親プロセス |
| 範囲 | `ENOENT`=2、`EACCES`=13 など | **0〜255 のみ** |
| 受け取り方 | `Err(Errno)` | `waitpid` の `WaitStatus::Exited(pid, code)` |

**子と親は別プロセスなので、親は `Errno` という Rust の値を受け取れない。**
死ぬときに渡せるのは 0〜255 の整数ひとつだけ。だから子が自分で翻訳して `_exit` に渡す。

`_exit(errno as i32)` と書くと動きはするが、受け取った親は
「2 が来た。errno の 2 のつもりなのか、コマンド自身が `exit(2)` したのか」を区別できない。

シェルの慣習に合わせるのが正解。**カーネルが決めたルールではなく業界の慣習**。

```rust
match execvp(&cmd, &args) {
    Err(Errno::ENOENT) | Err(Errno::ENOTDIR) => unsafe { _exit(127) }, // 見つからなかった
    _                                        => unsafe { _exit(126) }, // 見つかったが実行できなかった
}
```

境目は「そのファイルに辿り着けたかどうか」。`EACCES` / `EISDIR` / `ETXTBSY` などは全部 126 側。
`bash` で `nosuchcmd; echo $?` を打つと 127 が出る。

`execvp` の失敗理由は `ENOENT` だけではない(`man 2 execve` の ERRORS に
`EACCES` `EISDIR` `ENOEXEC` `ETXTBSY` `ELOOP` `ENAMETOOLONG` `EMFILE` `ENOMEM` など多数)。

なお `ENOEXEC` は観測できない。`man 3 exec` によると **`p` が付く関数
(`execvp` など)は `ENOEXEC` が返ると `/bin/sh` にそのファイルを渡して再実行する**。
`p` は `PATH` 解決以外にもう1つ仕事をしている。

### `main -> Result` は errno を終了コードに翻訳しない

```
$ ./target/debug/myrun nosuchcmd
Error: ENOENT              ← ENOENT は 2
[myrun] pid=24720 exited with 1    ← でも終了コードは 1
```

Rust の `main` が `Result` を返せるのは `Termination` トレイトによるもので、
`Err(e)` の場合は **`stderr` に `Error: {e:?}`(Debug 表示)を出して、
終了コードは `ExitCode::FAILURE` = 1 固定**。中身は反映されない。

`main` の `Result` は人間が読むメッセージを出すための道であって、
プロセス間で情報を渡す道ではない。

### `execvp` の戻り値が `Result<Infallible, Errno>` な理由

**成功したらこの関数から戻ってくることが原理的にありえない**から。
成功した瞬間にプロセスの中身が置き換わるので「`execvp` の次の行」が存在しない。
`Infallible`(値を作れない型)を `Ok` の中身に置いて「`Ok` は起こりえない」を型で宣言している。

実用上の意味は1つ: **`execvp` から制御が戻ってきた時点で、必ず失敗している。**

### `fork()` が `unsafe` な理由 ★Phase 1 で効いてくる

`man 2 fork` ではなく `nix::unistd::fork` の docs の Safety 節に書いてある。

> In a multithreaded program, only **async-signal-safe** functions like `pause` and `_exit`
> may be called by the child (the parent isn't restricted) until a call of `execve(2)`.
> Note that **memory allocation may not be async-signal-safe** and thus must be prevented.

理由は **`fork` が、呼んだスレッド1本しか複製しないから**。

```
【親】スレッドB が malloc のロックを取得中 ─┐ この瞬間に
【親】スレッドA が fork() を呼ぶ ───────────┘

【子】メモリはコピーされたので malloc のロックは「取得済み」状態
      でも、そのロックを解放するはずのスレッドB は子に存在しない
      → 子で malloc を呼ぶと永遠に待つ(デッドロック)
```

**ロックの状態はコピーされたのに、解放する主体だけが消えている。**
`malloc` のロックに限らず、`stdout` のロック、ロケール、乱数の内部状態など
グローバルな仕組みは全部同じ問題を持つ。

> **async-signal-safe**: シグナルハンドラの中から呼んでも壊れない関数。`_exit`、`write`、`execve` など
> ごく少数。一覧は `man 7 signal-safety`。fork 直後の子の制約がシグナルハンドラ内と同じなので
> 同じ言葉が使われている。

C の `fork` に `unsafe` が無いのは、C にその印を付ける仕組みが無いだけで危険性は同じ。

**現状の `myrun` はこの制約に違反している。** `fork` と `execvp` の間で
`CString::new` / `Vec` の確保を3回やっている。ただし `myrun` はシングルスレッドなので
実害は無い。**Phase 1 で tokio(マルチスレッド)を使うと本物のデッドロックになる。**

実務での形は「**`fork` の後、子は何もせず即 `exec` する**」。準備は `fork` より前に済ませる。
`std::process::Command` が中で `posix_spawn` を使おうとするのも、この地雷を避けるため。

---

## 分かったこと(Step 2: パイプ)

### パイプの EOF は「プロセスの終了」ではなく「fd の数」で決まる ★重要

`read` が `0` を返す条件は、**そのパイプの書き口を指している fd がシステム全体で1つも残っていないこと**。
「書き込み側のプロセスが終了したとき」ではない。ここが直感とずれる。

`fork` は fd テーブルごとコピーするので、何もしないと書き口の持ち主が勝手に2人になる。

```
pipe() 直後:   [書き口] ← 親が1本                  (計 1)
fork() 直後:   [書き口] ← 親が1本 / 子が1本         (計 2)  ← 勝手に増える
```

親が `drop(write_fd)` をサボると、子が `exec` して出力して終了した後も親自身が書き口を握ったままになる。
カーネルから見れば「まだ書き口の持ち主がいる。これから何か書かれるかもしれない」なので、
`read` は `0` を返さず**そこで永久にブロックする**。

> **パイプの端は、持っていること自体が意思表示になっている。** 使わないなら手放さないと、終わりが相手に伝わらない。

子側の `drop(read_fd)` は逆向きの理由。親が読み口を閉じたとき、子は「もう誰も読まない」と知る必要があり、
その通知は `write` の `EPIPE` / `SIGPIPE` という形で来る。これも**読み口の fd がゼロになったとき**に発生するので、
子が自分の分の読み口を握っていると条件が永遠に成立せず、子は誰も読まないパイプに書き続けて詰まる。

### パイプの容量は 64KiB。だから `read` を `waitpid` より先に回す ★重要

`man 7 pipe` の Pipe capacity。Linux は 64KiB。満杯になると書き手の `write` は
**親が読んで空きを作るまでブロックする**。

仮に親が「先に `waitpid`、後で `read`」の順だと:

```
子: 64KiB 書いた → パイプ満杯 → write でブロック(「親が読んでくれたら続きを書ける」)
親: waitpid でブロック          (「子が終了してくれたら次に進める」)
```

互いに相手の完了を待ってどちらも一生動かない(デッドロック)。
`echo hello` の 6 バイトでは**絶対に再現しない**のでテストをすり抜ける。
回帰テストには 64KiB を超える出力(`ls -la /usr/bin` = 72570 バイト)を使う。

### `read` の戻り値は「バッファに何バイト入ったか」

溜め込む側を `bytes.extend_from_slice(&buf)` と書いていて、毎回 8192 バイト固定で足していた。
正しくは `&buf[..m]`。`read` が 1 回で収まる出力(`echo hello`)では**露見しない**バグ。

`mycp` では `write(&buf[..m])` と書けていたのに、溜め込む形になった途端に間違えた。
**`read` の後で `buf` を触るときは常に `[..m]` で切る**、と覚える。

### 実測で確認したこと

| 壊し方                                  | `myrun echo hello` | `myrun ls -la /usr/bin` |
| --------------------------------------- | ------------------ | ----------------------- |
| 正しいコード                            | OK                 | OK (72570 バイト)       |
| 親の `drop(write_fd)` を外す            | **ハング**         | **ハング**              |
| `read` ループを `waitpid` の後ろに移す  | OK(通ってしまう)   | **ハング**              |

パイプ経由で受け取ったバイト列は、`ls -la /usr/bin` を直接実行した出力と**バイト単位で一致**した。

---

## 分かったこと(Step 3: 終了ステータスの分解)

### `waitpid` は「正常終了」と「シグナル死」を別物として返す。`$?` に落とすと区別が消える

`WaitStatus::Exited(pid, code)` と `WaitStatus::Signaled(pid, signal, core_dumped)` は別バリアント。
親プロセスは「どう死んだか」を完全に知っている。

一方、終了コードは 0〜255 の 8bit しかなく、シグナル死を表す場所が無い。
そこでシェルは **`128 + シグナル番号`** という慣習で `$?` に詰める(SIGSEGV=11 → 139)。
だから `sh -c 'exit 139'` と「SIGSEGV で死んだ」は **`$?` だけ見ると区別できない**。
情報は `waitpid` の時点では残っていて、`$?` に落とすときに失われる。

### `WaitStatus::Stopped` は `WUNTRACED` を渡さないと返らない

`man 2 waitpid` の options。デフォルトの `waitpid(pid, None)` は「終了」しか報告しない。
一時停止(SIGSTOP 等)まで拾うのはデバッガやシェルのジョブ制御の仕事なので、今は扱わない。

### コアダンプ = 死んだ瞬間のプロセスのメモリを丸ごとファイルに書き出したもの

`Signaled` の第3要素。**シグナルで死ぬ = 予期しない死**なので、後から解剖できるように
カーネルがプロセスのメモリ(全マッピング + レジスタ)を ELF 形式で吐き出す。それがコアダンプ。
`gdb <実行ファイル> <core>` で「死んだ瞬間」を覗ける。

- どのシグナルで吐くかは `man 7 signal` の **Action 列が `Core`** のもの(SIGSEGV / SIGABRT / SIGFPE / SIGBUS / SIGQUIT)。
  SIGTERM / SIGKILL は `Term` なので吐かない
- 実際に吐くかどうかは `ulimit -c`(`RLIMIT_CORE`)と `/proc/sys/kernel/core_pattern` で決まる。
  この VM は `core_pattern` が `|/usr/share/apport/apport ...` のパイプ型で、apport が受け取って
  `/var/lib/apport/coredump/` に置く
- 中身は `file` で見ると `ELF 64-bit LSB core file, x86-64, from 'sh -c kill -SEGV $$'`。
  `readelf -l` で LOAD セグメントが 25 個 = そのプロセスのメモリ領域が 25 個(`/proc/<pid>/maps` と対応)

> **Phase 6 のスナップショットは、これを VM 1台分でやるもの**(ゲストのメモリ + vCPU レジスタを丸ごと書き出す)。
> 「プロセスの死体」と「VM の静止画」は同じ発想。

---

## 分かったこと(Step 4: タイムアウトと `kill`) ※作業中

### シグナルハンドラは「別スレッド」ではなく「今のスレッドへの割り込み」

SIGALRM が届くと、カーネルは**今まさに動いていたスレッドを途中で止めて**、登録した関数に飛び込ませる。
呼ぶのはカーネルなので、関数は C の呼び出し規約で書く = `extern "C" fn(i32)`。
`CString` が「データの形を C に合わせる」なら、`extern "C"` は「関数の呼び方を C に合わせる」。

### async-signal-safe の正体は「スレッドセーフ」ではなく「途中がないこと」 ★重要

止められた側は「何かの真っ最中」かもしれない。`println!` の途中(stdout のロック保持中)で割り込まれ、
ハンドラが `println!` を呼ぶと、ロックの持ち主は「ハンドラの下で止まっている自分自身」なので永久に解放されない。

```
メイン: println! → stdout のロック取得 → 書き込み中 ← ここで SIGALRM
  └ ハンドラ: println! → ロック取得を試みる → 持ち主は止まっている自分 → デッドロック
```

`fork` のときと同じ形。「ロックを持ったまま二度と動かない誰かがいる」。
fork では「子に存在しないスレッド」、シグナルでは「ハンドラの下で止まっている自分」。`malloc` も同じ理由でダメ。

| | スレッドセーフ | シグナルセーフ |
| --- | --- | --- |
| `AtomicBool` | ○ | ○(途中がない) |
| `Mutex<bool>` | ○ | **×**(割り込まれた側がロックを持っているかも) |

ハンドラの仕事は「フラグを1つ立てる」だけにする。C の `volatile sig_atomic_t` に相当するのが Rust の `static AtomicBool`。

### `AtomicBool` に「途中がない」とは

ロックは「複数の CPU 命令にまたがる途中状態を、他人(他スレッド or ハンドラ)に見せない」ための道具。
`counter += 1` は「読む → 足す → 書く」の3命令なので途中がある。

`on_alarm` を release ビルドで逆アセンブルすると:

```
<on_alarm>:
   movb   $0x1, IS_SIGALARM_TRIGGERED(%rip)    ← CPU 命令1個
   ret
```

ハードウェアが「アラインされた1ワードの書き込みは半端な状態を見せない」と保証しているので、
守るべき「途中」が存在しない。守るものがないからロックがない。

| 道具 | 守り方 | できること |
| --- | --- | --- |
| `Mutex` | 「途中」の間、他人を**待たせる** | 複数の値・複数ステップをまとめて1つの操作にできる |
| `AtomicBool` | 「途中」を**作らない** | 1つの値に対する1回の操作だけ |

`fetch_add` のような読み書き込みも `lock add` 1命令に翻訳されるので同じ枠。
複数コアが本当に同時に動く今の CPU でも成り立つのはハードウェアの保証のおかげ。
→ Phase 3 で vCPU スレッドと HTTP スレッドの状態共有で再登場する。

### `EINTR` は「常に再試行」ではない

`mycp` の `read` ラッパーは `EINTR` を握りつぶして再試行していた。Step 2 まではそれで正しかった。
今回は SIGALRM 由来の `EINTR` が「時間切れの合図」なので、`EINTR` を受けたらフラグを見て分岐する。
時計で経過時間を測るのは間接的な推定で、「SIGALRM が来た」と知っているのはハンドラだけ。

`sigaction` に `SA_RESTART` を付けると `read` が勝手に再開されて `EINTR` が来なくなる(`man 7 signal` の
"Interruption of system calls")ので、今回は付けない(`SaFlags::empty()`)。

### `kill` の後にやること

1. **パイプを EOF まで読み切る** — タイムアウト前に子が書いた分がパイプに残っている。
   `break` すると捨ててしまう。`kill` 後は読み取りループに戻れば、子が死ぬ → 書き口が閉じる → EOF → `waitpid` へ自然に流れる
2. **`waitpid` する** — ゾンビ回避に加えて、「SIGTERM で本当に死んだのか、無視して生きているのか」は
   `waitpid` の結果でしか分からない。SIGKILL に切り替える判断材料

SIGTERM のデフォルト動作は子のコードを一切走らせずカーネルが即終了させる(フラッシュも無い)。
「子が SIGTERM を握って後始末する」場合だけ Step 2 の罠4(パイプ満杯で詰まる)が再発する。

### 6秒問題: `kill` の宛先は「1匹」だった ★Phase 1 で必ず踏む

```
$ time (MYRUN_TIMEOUT=2 myrun sh -c 'echo partial; sleep 6; echo done')
[myrun] timeout (2s), sending SIGTERM to pid=20127
[myrun] captured 8 bytes: "partial\n"
[myrun] pid=20127 killed by signal SIGTERM
real    0m6.003s      ← 2 秒ではなく 6 秒
```

`strace -f` で見ると `myrun → sh → sleep` の3段。`sh` は 2 秒で死んでいるが、
`sleep` は `sh` から fork+exec で**パイプの書き口を引き継いだまま**生きている。
書き口を握る fd が残っている限り EOF は来ない(Step 2 の法則)ので、`sleep` が自然死するまで親の `read` が戻らない。

`sh -c 'echo partial; sleep 10'` も同じ(dash は最後のコマンドも fork する。`strace -f` で確認済み)。

対処は**プロセスグループ**。`kill(pid)` はプロセス1つが宛先だが、グループ宛なら全員に届く。
子が `setpgid(0, 0)` で自分を先頭とする新グループを作り(PGID = 子の PID)、親は `killpg(child, SIGTERM)` で群れごと殺す。
`man 2 kill` の「pid が -1 より小さいとき」= 負の PID がグループ宛の印。

プロセスグループは孫が `setsid` で脱走できるので、本番のコンテナ基盤は cgroup / PID namespace を使う(Phase 5)。

---

## 次にやること

- [x] `fork` と `execvp` の間のメモリ確保(`CString::new`、`Vec`)を `fork` より前に移す
      → Step 2 で `pipe()` を `fork` の前に呼ぶ必要があり、自然に「fork 前の準備」ブロックができたので一緒に対応
- [x] Step 2: `pipe` で子の stdout を親が読み取る(罠1〜4 すべて実際にハングさせて確認した)
- [x] Step 3: 終了ステータスを分解する(正常終了 / シグナルで死んだ)
- [ ] Step 4: 時間内に終わらなければ子を殺す(`alarm` + `SIGALRM` ハンドラ + `kill`) ← 作業中(2026-09-14)
      - [x] `MYRUN_TIMEOUT=2 myrun sleep 10` → SIGTERM で殺して `$?`=143。タイムアウト前の出力も回収できる
      - [x] 6秒問題の原因を特定: 孫の `sleep` が書き口を握っている(上の「分かったこと」参照)
      - [ ] **次はここ**: プロセスグループで群れごと殺す。子で `setpgid(Pid::from_raw(0), Pid::from_raw(0))`
            (`fork` 後・`exec` 前)、親は `kill` を `killpg(child, SIGTERM)` に変える。
            確認: `time (MYRUN_TIMEOUT=2 myrun sh -c 'echo partial; sleep 10')` が 2 秒で戻ること、
            `strace -f -e trace=kill` で `kill(-<pid>, SIGTERM)` と負の PID が出ること
      - [ ] SIGTERM を無視する子(`sh -c 'trap "" TERM; sleep 10'`)への SIGKILL エスカレーション
- [ ] Step 5: `strace -f` で観察する
- [ ] `docs/glossary.md` に追加する: プロセス / `fork` / `exec` / 終了コード / async-signal-safe / ABI / パイプ / EOF / シグナル / コアダンプ / async-signal-safe と Atomic / プロセスグループ

### Phase -1 の残り(この演習以外)

- [ ] 仮想メモリ(EPT の理解に直結する)

---

## 自分の言葉で書く

> Step 5 を終えてから埋める。**書けないなら理解できていない。**

### `fork` と `exec` が分かれていることの意味

(未記入)

### `std::process::Command` は何を隠していたか

(未記入)
