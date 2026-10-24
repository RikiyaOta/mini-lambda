# Phase 2: OS-level 隔離

**日付**: 2026-10-22 開始
**目安**: 1週間(厳守。ROADMAP: 「深追いしない」)
**ゴール**: nsjail 的な最小サンドボックスが動き、ベンチの数字が1行増えている

## ステップ

| #   | やること                                                                    | 状態             |
| --- | --------------------------------------------------------------------------- | ---------------- |
| 1   | PID namespace(`experiments/oslearn/src/bin/myns.rs`)。子が自分を PID 1 と見る | 🔶 10-23 sudo なしで動いた |
| 2   | mount namespace + `pivot_root` で最小 rootfs に切り替える                    | -                |
| 3   | cgroup v2: `memory.max` / `cpu.max` / `pids.max`                            | -                |
| 4   | seccomp: 全部 deny → 動くまで許可を足し、必要だった syscall 数を数える       | -                |
| 5   | `crates/sandbox-ns` として `Sandbox` に登録し、ベンチを流す                  | -                |

## メモ

- 新しい環境(LMDE 7)では `userfaultfd-sys` のビルドに libclang が要る → `sudo apt install libclang-dev`
- `sudo ./target/debug/myns` のように、ビルド済みバイナリを直接 `sudo` する(`sudo cargo run` にしない)

## 説明の控え(10-23、エージェントが書いたもの)

### namespace は種類ごとに別々のオブジェクト

1つの namespace が複数の性質を持つのではない。プロセスは種類ごとの欄を持ち、それぞれが1つの namespace を指す。
`unshare` に渡したフラグの種類だけ、新しいオブジェクトに差し替わる。書かなかった種類はホストと共有のまま。

```
子プロセス
  user   → 新しい user namespace   ← CLONE_NEWUSER で作った
  pid    → 新しい PID namespace    ← CLONE_NEWPID で作った
  mnt    → ホストと同じもの
  net    → ホストと同じもの
  uts / ipc / ... → ホストと同じもの
```

実物は `ls -l /proc/<pid>/ns/` で見える(`pid:[4026531836]` の番号が namespace の識別子)。

### `unshare` を呼んだ本人が「いつ入るか」は種類で違う

作られるのはどれも `unshare` の時点。

| 種類 | `unshare` を呼んだ本人 | その後に `fork` した子 |
| ---- | ---------------------- | ---------------------- |
| User | すぐに入る             | 入る                   |
| PID  | 入らない(PID は途中で変えられない) | 入る(最初の子が PID 1) |

### なぜ `sudo` なしでも PID namespace を作れたか

user 以外の namespace は、作られるときに「持ち主の user namespace」が決まり、作る権限はその持ち主の中での権限で判定される。

```
ホストの user namespace(最初からあるもの)
 └ 新しい user namespace   ← 作った本人はここで CAP_SYS_ADMIN を持つ
     │ 「所有」
     ▼
   新しい PID namespace     ← 親はホストの PID namespace
```

1. 同じ `unshare` 呼び出しの中で、user namespace が必ず先に作られ(`man 7 user_namespaces`)、本人はすぐその中に入る
2. 続けて PID namespace を作るとき、持ち主は新しい user namespace。そこでは `CAP_SYS_ADMIN` を持っているので許可される
3. `CLONE_NEWPID` だけのときは、持ち主がホストの user namespace で、そこでは `CAP_SYS_ADMIN` が無い → EPERM

中で手に入る権限は、その namespace の中だけで有効。外(ホスト)に対しては何の力も無い(`ps` で見ると `rikiyaota` のまま)。

### uid が 65534 になる理由

`uid_map`(中の uid と外の uid の対応表)を書いていないと、カーネルは対応する番号が無いものとして 65534(nobody)を見せる。
`unshare --map-root-user` は「中の 0 = 外の 1000」を書いてくれていた。

## 分かったこと

<!-- 自分の言葉で。上の「説明の控え」を見ずに書けるかを試すとよい -->
