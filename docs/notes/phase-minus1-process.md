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
| 1    | `fork` + `exec` + `waitpid` でコマンドを実行し終了ステータスを表示 | プロセス生成、PID、ゾンビ   | ✅ 完了             |
| 2    | `pipe` で子の stdout を親が読み取る                     | fd の付け替え、EOF          | ⬜ **次はここから** |
| 3    | 終了ステータスを分解する(正常終了 / シグナルで死んだ)   | シグナル                    | ⬜                  |
| 4    | 時間内に終わらなければ子を殺す                          | `kill`、SIGTERM と SIGKILL  | ⬜                  |
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

## 次にやること

- [ ] `fork` と `execvp` の間のメモリ確保(`CString::new`、`Vec`)を `fork` より前に移す
      → 今は動くが Phase 1 の tokio で効いてくる。Step 2 のついででよい
- [ ] Step 2: `pipe` で子の stdout を親が読み取る
      - 仕様: 子の stdout だけをパイプに繋ぎ、親が EOF まで読んで表示する(stderr は親のまま)
      - API: `nix::unistd::pipe() -> Result<(OwnedFd, OwnedFd)>` (読み口, 書き口)、
        `nix::unistd::dup2_stdout<Fd: AsFd>(fd) -> Result<()>`、`nix::unistd::read`
      - 罠1: `fork` するとパイプの両端が親にも子にも存在する。使わない側を閉じる
      - 罠2: 1をサボると親の `read` が `0`(EOF)を返さなくなる(`man 2 pipe`)
      - 罠3: fd は `exec` を越えて引き継がれる
      - 罠4: 親が「先に `waitpid` してから `read`」の順だと詰まる。`myrun ls -la /usr/bin` のような
        大きい出力で試すこと
- [ ] Step 3〜5(上の表の通り)
- [ ] `docs/glossary.md` に追加する: プロセス / `fork` / `exec` / 終了コード / async-signal-safe / ABI

### Phase -1 の残り(この演習以外)

- [ ] 仮想メモリ(EPT の理解に直結する)

---

## 自分の言葉で書く

> Step 5 を終えてから埋める。**書けないなら理解できていない。**

### `fork` と `exec` が分かれていることの意味

(未記入)

### `std::process::Command` は何を隠していたか

(未記入)
