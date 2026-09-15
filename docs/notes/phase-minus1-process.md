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

| Step | やること                                                           | 押さえるもの                            | 状態           |
| ---- | ------------------------------------------------------------------ | --------------------------------------- | -------------- |
| 1    | `fork` + `exec` + `waitpid` でコマンドを実行し終了ステータスを表示 | プロセス生成、PID、ゾンビ               | ✅ 完了(09-09) |
| 2    | `pipe` で子の stdout を親が読み取る                                | fd の付け替え、EOF                      | ✅ 完了(09-13) |
| 3    | 終了ステータスを分解する(正常終了 / シグナルで死んだ)              | シグナル                                | ✅ 完了(09-14) |
| 4    | 時間内に終わらなければ子を殺す                                     | `kill`、SIGTERM と SIGKILL              | ✅ 完了(09-15) |
| 5    | `strace -f` で観察する                                             | `clone`/`execve`/`pipe2`/`dup2`/`wait4` | ✅ 完了(09-15) |

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

# Step 4 以降: MYRUN_TIMEOUT 秒で SIGTERM、1 秒待って SIGKILL。孫プロセスまで巻き込む
time (MYRUN_TIMEOUT=2 ./target/debug/myrun sh -c 'echo partial; sleep 10')       # → 2 秒 / captured "partial\n" / 143
time (MYRUN_TIMEOUT=2 ./target/debug/myrun sh -c 'trap "" TERM; sleep 10')       # → 3 秒 / SIGKILL / 137
MYRUN_TIMEOUT=2 ./target/debug/myrun sh -c 'echo ok'                             # → 時間内なら今まで通り
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

| 実行             | 症状                                                      |
| ---------------- | --------------------------------------------------------- |
| `ls -la /tmp`    | 出力は出るが `-la` が消えている(**成功したように見えた**) |
| `echo hello`     | 何も出ない(空行だけ出ていた)                              |
| `sh -c 'exit 3'` | よく分からないエラー                                      |

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

|              | errno                                     | 終了コード (exit status)                     |
| ------------ | ----------------------------------------- | -------------------------------------------- |
| 何を伝えるか | システムコールが失敗した理由              | プロセスがどういう結末で終わったか           |
| 誰から誰へ   | カーネル → syscall を呼んだプログラム自身 | 死ぬプロセス → 親プロセス                    |
| 範囲         | `ENOENT`=2、`EACCES`=13 など              | **0〜255 のみ**                              |
| 受け取り方   | `Err(Errno)`                              | `waitpid` の `WaitStatus::Exited(pid, code)` |

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

| 壊し方                                 | `myrun echo hello` | `myrun ls -la /usr/bin` |
| -------------------------------------- | ------------------ | ----------------------- |
| 正しいコード                           | OK                 | OK (72570 バイト)       |
| 親の `drop(write_fd)` を外す           | **ハング**         | **ハング**              |
| `read` ループを `waitpid` の後ろに移す | OK(通ってしまう)   | **ハング**              |

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

## 分かったこと(Step 4: タイムアウトと `kill`)

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

|               | スレッドセーフ | シグナルセーフ                                |
| ------------- | -------------- | --------------------------------------------- |
| `AtomicBool`  | ○              | ○(途中がない)                                 |
| `Mutex<bool>` | ○              | **×**(割り込まれた側がロックを持っているかも) |

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

| 道具         | 守り方                           | できること                                        |
| ------------ | -------------------------------- | ------------------------------------------------- |
| `Mutex`      | 「途中」の間、他人を**待たせる** | 複数の値・複数ステップをまとめて1つの操作にできる |
| `AtomicBool` | 「途中」を**作らない**           | 1つの値に対する1回の操作だけ                      |

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

### SIGTERM → SIGKILL の2段階

`sh -c 'trap "" TERM; sleep 10'` に SIGTERM を送っても誰も死なない。**「無視」に設定されたシグナルは
`fork` と `exec` を越えて引き継がれる**(`man 2 execve` の Signals)ので、`sleep` も無視する。
EOF が永遠に来ず、親は `read` で固まる(外側を `timeout 5` で囲うと 124 で殺される)。

SIGKILL は "cannot be caught, blocked, or ignored"(`man 7 signal`)。これが SIGTERM(お願い)と
SIGKILL(強制)の使い分けの根拠。Phase 1 の関数強制終了も同じ2段階になる。

実装は「1回目のアラーム / 2回目のアラーム」の状態を親のループに持ち、1回目で SIGTERM + フラグ戻し + `alarm(1)`、
2回目で SIGKILL。EOF が来たら `alarm::cancel()` で猶予アラームを解除する(残すと後続の `waitpid` を割り込む)。

```
11845 alarm(2)                = 0
11845 kill(-11846, SIGTERM)   = 0     ← 2 秒後。グループ宛
11845 alarm(1)                = 0     ← 猶予
11845 kill(-11846, SIGKILL)   = 0     ← 3 秒後
11846 +++ killed by SIGKILL +++       ← sh
11847 +++ killed by SIGKILL +++       ← sleep(孫)
11845 alarm(0)                = 0     ← cancel
```

### ラッパー自身の失敗は 125

`setpgid` の失敗は「コマンドの側」の問題ではなく `myrun` 自身の準備の失敗。126/127 にすると
呼び出し元が「コマンドが壊れている」と誤解する。`timeout` / `env` / `nice` / `nohup` は
**125 = the command itself fails** で統一している(`timeout --help` の Exit status)。

`timeout` はタイムアウト時に 124 を返し、`--preserve-status` で子の死因(143)に切り替えられる。
`myrun` は 143 のまま。「呼び出し元が『時間切れ』と『シグナル死』を区別したいか」で決まる設計判断で、
Phase 1 で HTTP ステータスに翻訳するときに再考する。

---

## Step 5: `strace -f` で全体を読む(自分で埋める)

取得コマンド:

```bash
cd experiments/oslearn
MYRUN_TIMEOUT=2 strace -f -y -o /tmp/myrun.strace ./target/debug/myrun sh -c 'echo hi; sleep 10'
```

登場人物(自分のログの PID に置き換える):

| PID   | 誰          | 生まれ方                    |
| ----- | ----------- | --------------------------- |
| 15393 | `myrun`(親) | シェルが起動                |
| 15394 | `sh`(子)    | 15393 の `clone` → `execve` |
| 15395 | `sleep`(孫) | 15394 の `clone` → `execve` |

### syscall と自分のコードの対応

`myrun.rs` の行番号は書いた時点のもの。Rust で呼んだ名前と実際の syscall 名がずれているものは「正体」欄に書く。

| ログ行 | PID   | syscall                        | `myrun.rs` の対応                             | 正体・気づいたこと                                                                         |
| ------ | ----- | ------------------------------ | --------------------------------------------- | ------------------------------------------------------------------------------------------ |
| 61     | 15393 | `pipe2([3, 4], 0)`             | `pipe()`                                      |                                                                                            |
| 62     | 15393 | `clone(flags=...\|SIGCHLD)`    | `fork()`                                      |                                                                                            |
| 67     | 15393 | `close(4)`                     | `drop(write_fd)`                              | `drop` 自体はただのRustのメモリ管理の話だが、OwnedFd が裏でちゃんとcloseをしてくれている。 |
| 69     | 15393 | `rt_sigaction(SIGALRM, ...)`   | `sigaction(Signal::SIGALRM, &action)?`        |                                                                                            |
| 72     | 15394 | `close(3)`                     | `drop(read_fd)`                               |                                                                                            |
| 73     | 15393 | `alarm(2)`                     | `set(timeout)`                                | 直後に SIGTERM を送った形跡はないので、単に set しただけだと推測した。                     |
| 76     | 15394 | `dup2(4, 1)`                   | `dup2_stdout()`                               |                                                                                            |
| 77     | 15393 | `read(3, ...)`                 | `read`                                        |                                                                                            |
| 81     | 15394 | `setpgid(0, 0)`                | `setpgid(Pid::from_raw(0), Pid::from_raw(0))` |                                                                                            |
| 82〜   | 15394 | `execve(...) = -1 ENOENT` ×N   | `execvp(&cmd, &args)`                         | PATHのディレクトリを先頭から順に試して ENOENT なら次へ、と進んでいる。                     |
| 92     | 15394 | `execve(...) = 0`              | `execvp(&cmd, &args)`                         | コマンド実行が成功した（ちゃんと見つかった）                                               |
|        | 15394 | `clone` / `execve("sleep")`    | (自分のコードではない)                        |                                                                                            |
|        | 15394 | `wait4(...)`                   | (自分のコードではない)                        |                                                                                            |
| 272    | 15393 | `read` → `ERESTARTSYS`         | `read`                                        | ブロック中の read が中断された。もし `SA_RESTART` がついていれば再開される、保留状態。     |
| 273    | 15393 | `--- SIGALRM ---`              |                                               | SIGALRM が配達された。この行と次の行の間で、on_alarmが走って、フラグが切り替わる。         |
| 274    | 15393 | `rt_sigreturn` → `EINTR`       |                                               | rt_sigreturn はハンドラからもどる syscall。ここで read の結果が EINTR に確定する。         |
| 276    | 15393 | `kill(-15394, SIGTERM)`        | `killpg(child, Signal::SIGTERM)`              | プロセスグループに対して送る場合は、プロセス番号にマイナスがつくというルール。             |
|        | 15394 | `--- SIGTERM {si_pid=...} ---` |                                               | 子プロセスがSIGTERMを受け取った。                                                          |
|        | 15395 | `--- SIGTERM {si_pid=...} ---` |                                               | 孫プロセスが SIGTERM を受け取った。                                                        |
|        | 15393 | `read(3, ...) = 0`             |                                               | 親がEOFまで読み切った。                                                                    |
|        | 15393 | `alarm(0)`                     | `cancel`                                      |                                                                                            |
|        | 15393 | `wait4(15394, ...)`            | `waitpid`                                     |                                                                                            |
|        | 15393 | `exit_group(143)`              | `exit(128 + signal as i32)`                   |                                                                                            |

### 読んで気づいたこと(3〜5個、自分の言葉で)

- 親プロセス、子プロセスの処理がプロセス番号でわかるようになっている。
- `nix` の関数がそのまま syscall の名前に対応しているわけではないが、意味はわかりやすいようになっている。
- `execvp` はPATHを意識しなくていいので便利だが、そのコストは syscall の回数に跳ねている。
- sh が sleep を起動しているので、孫プロセスも起動している。

### `std::process::Command` との違い(余裕があれば)

```bash
# 比較用: 同じことを Command でやったら何が違うか
strace -f -o /tmp/cmd.strace sh -c 'echo hi'     # 手元の適当な Rust プログラムでも可
```

- 1行目で `execve` が一発で成功している。なんでだ？シェルが補完してる？
  → **`strace` 自身が PATH を解決してから `execve` している**(トレース対象は解決後のプロセスなので探索は写らない)。シェルではない
- 49行目で write がされているが、子プロセスを起動することなく、そのプロセス自身が write を実行している。なんでだ？
  → **`echo` は `sh` の組み込みコマンド(builtin)** なので `fork` しない。`sleep` は外部コマンドなので `fork` + `execve` する。
  `myrun.strace` で `sh` が `echo hi` のために `clone` していなかったのも同じ理由
- ※ 上の2つは `sh` 単体を strace したもので、`std::process::Command` の比較はまだ(テンプレのコマンドが紛らわしかった)。
  やるなら `Command::new("sh").args(["-c", "echo hi; sleep 10"]).output()` の3行プログラムを strace する。
  見どころ: `clone` ではなく `clone3(CLONE_VM|CLONE_VFORK)` か `posix_spawn` が出るか / `pipe2` に `O_CLOEXEC` が付くか / `execve` の前に何を閉じているか

- `stdcmdsh.rs` という簡易なスクリプトを実装した。
    - これは `clone3()` で子プロセスを生成しているらしい。
    - man clone3 を見ると、clone は glibc でラッパーが提供されていたりするが、clone3 はそのシステムコールの新しいバージョンって感じらしい。
    - `pipe2([4,5], O_CLOEXEC)`, `pipe2([6,7], O_CLOEXEC)` が実行されている。これは何をしてるんだろう？
        - → **stdout 用と stderr 用の2本**。`.output()` は両方キャプチャするから。stdin は `/dev/null` を開いて(fd=3)割り当てている(親の stdin を継承させない)。myrun は stdout 1本だけ・stdin は継承、という違い

### 大事だった3点(まとめ)

**(1) `O_CLOEXEC` が myrun の `drop` の代わり** ★

- `O_CLOEXEC` = 「この fd は `execve` した瞬間にカーネルが自動で閉じる」印。myrun で子の中に書いた `drop(read_fd)` を、カーネルにやらせている
- ただし **`dup2` で複製した fd には `O_CLOEXEC` が付かない**(`man 2 dup`: "does not set the close-on-exec flag")。
  だから `dup2(3,0)` `dup2(5,1)` `dup2(7,2)` で作った 0/1/2 だけが exec を生き延び、元の 3〜7 は exec で消える。
  **「残したいものだけ `dup2` する」**という設計
- 親側は自動では閉じないので `clone3` 直後に `close(3)` `close(5)` `close(7)` を明示している。
  EOF が来るのはこの close があるから = Step 2 の「fd の本数が 0 になって初めて EOF」と同じ規則

**(2) `clone3(CLONE_VM|CLONE_VFORK|CLONE_CLEAR_SIGHAND)` = メモリをコピーしない fork** ★

| フラグ                | 意味                                                            |
| --------------------- | --------------------------------------------------------------- |
| `CLONE_VM`            | 親とメモリ空間を**共有**する(コピーしない)                      |
| `CLONE_VFORK`         | 子が `execve` か `_exit` するまで**親を止める**                 |
| `CLONE_CLEAR_SIGHAND` | シグナルハンドラを全部デフォルトに戻す                          |
| `exit_signal=SIGCHLD` | 死んだとき親に SIGCHLD を送る(= 普通の子プロセスとして扱われる) |

- `fork` はアドレス空間をコピーする。CoW でも**ページテーブルのコピー**は実際に起きるので、親のメモリが大きいほど遅い。
  どうせ直後の `execve` で全部捨てるのに。だから `CLONE_VM` で共有してしまう
- 共有したままだと子が親のメモリを壊せる。そこで `CLONE_VFORK` で「子が exec するまで親を凍結」して安全を確保する。
  トレースで `clone3` が `<unfinished ...>` のまま子の `execve` 成功まで再開しないのはこれ
- 代償として **clone3〜execve の間にできることは極端に少ない**(myrun の「子では `_exit` 以外呼ぶな」の厳しい版)。
  実際に子がやっているのは `rt_sigaction` / `dup2` / `rt_sigprocmask` / `execve` だけ
- `CLONE_CLEAR_SIGHAND` は Step 4 で確認した「無視設定は fork と exec を越えて残る」問題への対処
- **Phase 3 への橋**: 「プロセス生成が重いのはアドレス空間のコピーだから」は、そのまま
  「VM 起動が重いのはゲストメモリの用意だから」に対応する。スナップショット復元が速い理由もここ

**(3) 読み口が2本になると単純な `read` ループは使えない** ★

```
ioctl(4, FIONBIO, [1])   ← non-blocking にする
ioctl(6, FIONBIO, [1])
poll([{fd=4, POLLIN}, {fd=6, POLLIN}], 2, -1)
```

- myrun は読み口が1本だったので `read` でブロックしてよかった。2本あると、stdout を read でブロックしている間に
  stderr のパイプが 64KiB で埋まって子が止まる = **Step 2 で踏んだデッドロックが再発する**
- だから両方 non-blocking にして `poll` で「読める方」を待つ。最後に `POLLHUP` が2つ返る(= 両端 EOF)
- **「読み切ってから `wait4`」の順序は myrun と同じ**

おまけ:

- `sh` は `sleep` を `vfork` で起動している(dash も同じ最適化をしている)
- `execve` の PATH 探索が11回 ENOENT で失敗して `/usr/bin/sh` で成功しているのが見える。
  前回 `sh` 単体を strace したときに見えなかったのは strace が自分で PATH を解決していたからで、
  今回は Rust 側が自前で探索しているのでちゃんと写っている

---

## 次にやること

- [x] `fork` と `execvp` の間のメモリ確保(`CString::new`、`Vec`)を `fork` より前に移す
      → Step 2 で `pipe()` を `fork` の前に呼ぶ必要があり、自然に「fork 前の準備」ブロックができたので一緒に対応
- [x] Step 2: `pipe` で子の stdout を親が読み取る(罠1〜4 すべて実際にハングさせて確認した)
- [x] Step 3: 終了ステータスを分解する(正常終了 / シグナルで死んだ)
- [x] Step 4: 時間内に終わらなければ子を殺す(`alarm` + SIGALRM ハンドラ + `killpg`、SIGTERM→SIGKILL の2段階)
- [x] Step 5: `strace -f` で観察する(表は上の「Step 5」節。空欄は残っているが十分)
- [x] (任意)`std::process::Command` 版を strace して比較する(`stdcmdsh.rs` / 09-15)
- [x] **myrun 卒業**: 末尾の「自分の言葉で書く」2枠を埋めた(09-15)。これで演習は完了
- [x] `docs/glossary.md` に「プロセスとシグナル」の節を追加(11語、2026-09-15)

### Phase -1 の残り(この演習以外)

- [ ] 仮想メモリ(EPT の理解に直結する)

---

## 自分の言葉で書く

> Step 5 を終えてから埋める。**書けないなら理解できていない。**

### `fork` と `exec` が分かれていることの意味

- `fork` はプロセスをコピーして新しい子プロセスを生成する。
    - その時、メモリ空間をコピーする。スレッドは、現在のスレッドしかコピーしない。
    - なので、たとえば他のスレッドがロックを獲得している時に `fork` してしまうと、子プロセスでは、誰も解放することのないロックを待つ状況になってしまうことがある。
    - そのため、`fork` した後は基本的にすぐ `exec` するべき。
    - たとえば `println!` なども stdout をロックするので、実行してはダメ。
- `exec` は現在のプロセスを指定したプログラム（コマンド）で上書きして実行する。
    - 子プロセスを実行するというのは、`fork` してから `exec` するという2段階になっている。
    - ２段階にすると何が嬉しいのか？
    - `myrun`では、子プロセス側では `fork` と `exec` の間で、以下のような処理をしている：
        - `drop(read_fd)`
        - `dup2_stdout(write_fd)`
        - `setpgid(0,0)`
    - つまり、単に子プロセスを実行するだけでなく、不要な fd を閉じたり、標準出力をリダイレクトしたり、プロセスグループを定義している。
    - `fork`+`exec` が1つのシステムコールになっていたら、上記の3行を実行する隙間がなくなる。
    - プロセスをフォークしてから実行するまでの間の細かな制御やリソース管理を行うことができるのかな。
    - ※あるいは、`fork`+`exec`を1つのシステムコールにして、引数にあり得るパターンを網羅したオプションを定義することも可能かもしれないが、API設計者があらかじめ全て予想する必要があり、困難。`fork`と`exec`が分かれているということは、「普通のコードで書いてくれ」という意図と理解できる。

### `std::process::Command` は何を隠していたか

- myrun と比較して、何を書かずに済んでいるか？で考える。
- Command が肩代わりしていたのは、
    - `sh` など、子プロセスで実行する引数を CString に変換する処理。
    - 子プロセスの明示的な `fork`, `exec`。`pipe` もやってくれてる。
    - `waitpid` もやってくれてる。
- でも肩代わりしていないのは、
    - SIGTERM を送る手段（ `std::process::Child` には `kill()` だけある）。
    - タイムアウトのような、いつ殺すかを決める仕組み。
- だから myrun を書いた意味は、
    - 「最初に SIGTERM を送り、猶予を持たせて SIGKILL する」という殺し方を実現できた。
