# Phase -1: システムコールでファイルコピー (mycp)

**日付**: 2026-09-04
**成果物**: `experiments/oslearn/src/bin/mycp.rs`

『ふつうのLinuxプログラミング』を一通り読み流した後、実際に手を動かすための演習。
ROADMAP の Phase -1「到達点の確認」3項目を、1本の演習で回収することを狙っている。

---

## 演習の全体設計(全5ステップ)

| Step | やること | 回収する到達点 | 状態 |
| ---- | ------------------------------------------------------------- | -------------- | ------ |
| 1 | `nix` の `open`/`read`/`write`/`close` だけでコピーを書く | 到達点1 | ✅ 完了 |
| 2 | わざと壊す(戻り値を無視する、部分読み書きを踏む) | 到達点1 | ✅ 完了 |
| 3 | `strace` で発行されたsyscallを読む | 到達点1, 2 | ⬜ **次はここから** |
| 4 | バッファサイズを 1B / 4KB / 64KB で変えて時間を測る | 到達点3 | ⬜ |
| 5 | `std::fs::copy` と `cp` を strace して比較する | 到達点2 | ⬜ |

到達点(ROADMAP より):

1. `std::fs::copy` を使わずにファイルコピーを書き、`strace` で発行されたシステムコールを説明できる
2. 「なぜアプリはディスクを直接触れないのか」を自分の言葉で説明できる
3. `BufReader` が存在する理由を、システムコールのコストの観点で説明できる

---

## 環境

作業は **GCP `vmm-dev` インスタンス上で直接**行っている(Claude Code もこのインスタンス上で動いている)。

| 項目 | バージョン |
| ------ | ---------- |
| cargo | 1.98.1 |
| rustc | 1.98.1 |
| strace | 6.8 |
| nix | 0.31.3 (`features = ["fs"]`) |

`man 2 read` などのセクション2 man page は導入済み。

### ディレクトリ

```
experiments/oslearn/
├── Cargo.toml
└── src/bin/mycp.rs     # Phase -1 では bin を足していく形にした
```

### 動作確認コマンド

```bash
cd experiments/oslearn
head -c 10M /dev/urandom > /tmp/src.bin
rm -f /tmp/dst.bin        # ← 消してから実行しないと mode が反映されない(後述)
cargo run --bin mycp -- /tmp/src.bin /tmp/dst.bin
cmp /tmp/src.bin /tmp/dst.bin && echo OK
```

---

## 分かったこと

### `open` の第3引数 `mode` の罠

最初 `Mode::S_IWUSR` だけを渡していたため、`0o200`(所有者に書き込みのみ)でファイルが作られ、
**自分で作ったファイルを自分で読めない**状態になった。`cmp` が `Permission denied` で
失敗していたのに、`sudo cmp` で確認してしまい成功したと誤認した。

- `mode` は **新規作成のときにしか適用されない**。既存ファイルを上書きする場合は既存のパーミッションのまま
- 実際に作られるのは `mode & ~umask`(この環境の umask は `0002`)
- 検証コマンドを `sudo` で実行すると、権限の問題を握りつぶしてしまう

> ⬜ **未実施の実験**: `chmod 600` したファイルに上書き実行して、パーミッションが変わらないことを確認する

### `OwnedFd` と `close`

`nix::fcntl::open` は `std::os::fd::OwnedFd`(nix の型ではなく標準ライブラリの型)を返す。
スコープを抜けると `Drop` で自動的に `close` される。C では `close` 忘れが fd リークになるが、
Rust では所有権が引き受けている。`nix::unistd::close` の引数が `IntoRawFd` で
所有権を奪う形になっているのも、二重 close を型で防ぐため。

### `read` と `write` の非対称性 ★重要

どちらも「要求より少ないバイト数を返すことがある」が、**意味が違う**。

- **`read` が `n` バイト返した** → 正常。残りは次の周回で読めばよい。`&buf[..n]` で足りる
- **`write` が `m` バイト返した (`m < buf.len()`)** → **書き残しが `buf.len() - m` バイトある**。
  ここで次の `read` に進むとそのデータは永久に失われる。**その場で書き切るループが要る**

これが `std::io::Write::write_all` の正体。生の `write` は頼んだ分を書くとは限らないのでラッパが要る。

> regular file 相手ではまず起きないので、対処しなくても動いてしまう。パイプ相手だと破綻する。

### `EINTR`

ブロック中の `read`/`write` にシグナルが届くと、カーネルは処理を打ち切って `EINTR` を返す。
これは**失敗ではなく「やり直して」の意味**なので、`?` で上に投げてはいけない。

仕様上の重要な保証:

> **`read`/`write` が `EINTR` を返すのは、1バイトも転送していない場合だけ。**
> 一部を転送した後にシグナルが届いた場合は、`EINTR` ではなく**転送済みのバイト数**が返る。

このおかげで「`EINTR` = バッファ全体を渡し直す」「`Ok(m)` = m バイト進んだ」と綺麗に二分でき、
二重書き込みにならない。ここが曖昧だと EINTR 処理はどこかで必ず壊れる。

- `sigaction` で **`SA_RESTART`** を付けるとカーネルが自動で再開するため `EINTR` は上がってこない。
  普段出会わないのはこのため
- ただし `SA_RESTART` を付けても再開されない syscall がある(`man 7 signal` の
  "Interruption of system calls and library functions by signal handlers" に一覧)

### 末尾再帰は Rust では最適化が保証されない

`write_all` を再帰で書いたので、実際にどうコンパイルされるか逆アセンブルして確認した。

```bash
objdump -d --disassemble=<write_all のシンボル> target/debug/mycp   | grep -c 'call.*write_all'
# => 2   (debug: 自分自身への call が2箇所 = 本物の再帰。スタックが積まれる)
objdump -d --disassemble=<write_all のシンボル> target/release/mycp | grep -c 'call.*write_all'
# => 0   (release: 最適化でループに変換された)
```

末尾再帰なので原理的にはループに変換できるが、**Rust は末尾呼び出し最適化を保証しない**。
debug ビルドでは `call` が残る。「入力によって深さが決まる再帰」は、コンパイラの気分に
安全性を預けることになるのでループで書くのが定石。

Step 4 でバッファサイズを 1MB などに変えると、最悪 100万段の再帰になりスタックオーバーフローしうる。

---

## 次にやること

- [ ] `write_all` の再帰をループに書き換える(上記の理由)
- [ ] Step 3: `strace ./target/debug/mycp /tmp/src.bin /tmp/dst.bin` で発行されたsyscallを読む
      - `open` ではなく **`openat`** が出てくるはず。なぜか
      - `read`/`write` の回数がファイルサイズ ÷ バッファサイズになっていることの確認
- [ ] Step 4: バッファサイズを 1B / 16B / 4KB / 64KB に変えて、syscall回数(`strace -c`)と実時間を測る
      - **結果は `results/` に CSV で残す**
      - → 到達点3(`BufReader` が存在する理由)の答えになる
- [ ] Step 5: `std::fs::copy` と `cp` コマンドを strace して比較(`copy_file_range` が見えるはず)
      - → 到達点2、および Phase 6(スナップショット、ページキャッシュ)への伏線
- [ ] 未実施の実験: `mode` は新規作成時にしか効かないことの確認

### Phase -1 の残り(ファイルコピー以外)

- [ ] プロセスと fork/exec、シグナル、パイプ
- [ ] 仮想メモリ(EPT の理解に直結する)

---

## 自分の言葉で書く(到達点の確認)

> Step 3〜5 を終えてから、ここを埋める。**書けないなら理解できていない。**

### なぜアプリはディスクを直接触れないのか

(未記入)

### `BufReader` が存在する理由を、システムコールのコストの観点で

(未記入 — Step 4 で測った数字を根拠にすること)

### `strace` で見えたシステムコールの説明

(未記入)
