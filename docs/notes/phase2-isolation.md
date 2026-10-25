# Phase 2: OS-level 隔離

**日付**: 2026-10-22 開始
**目安**: 1週間(厳守。ROADMAP: 「深追いしない」)
**ゴール**: nsjail 的な最小サンドボックスが動き、ベンチの数字が1行増えている

## ステップ

| #   | やること                                                                    | 状態             |
| --- | --------------------------------------------------------------------------- | ---------------- |
| 1   | PID namespace(`experiments/oslearn/src/bin/myns.rs`)。子が自分を PID 1 と見る | 🔶 10-25 uid_map / gid_map まで |
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

## 説明の控え(10-25、エージェントが書いたもの)

### 対応表は namespace に1つ

`uid_map` / `gid_map` はプロセスごとではなく、**user namespace ごとに1つ**。`/proc/self/uid_map` は「自分が属する user namespace の対応表」を見せる窓。

- `unshare(CLONE_NEWUSER)` の直後、呼んだ本人はもう新しい user namespace にいる(対応表は空 → uid は 65534)
- 本人が `/proc/self/uid_map` に書く → その namespace の対応表が決まる → 本人の uid も 0 になる
- `fork` した子は同じ user namespace にいるので、同じ対応表を使う(親子とも uid 0)

書き方は `<中の id> <外の id> <個数>` の1行。`sudo` なしで書けるのは**自分の uid / gid 1個だけ**なので、外の値は `unshare` の**前**に `getuid()` / `getgid()` で取っておく(後だと 65534)。どちらも1回しか書けない。

### `gid_map` の前に `setgroups` を `deny` にする理由

**グループはプロセスの持ち物。** ログイン時に `/etc/group` からコピーされ、`fork` で子に引き継がれる。
`id` の `gid=` がメインのグループ、`groups=` が**補助グループ**。ファイルを開くとき、カーネルはこのプロセスの一覧で権限を判定する。
`setgroups(2)` はこの補助グループの一覧を書き換えるシステムコールで、普通は `CAP_SETGID` が要る。

**危ない例**: グループだけを締め出すファイル

```
-rwx---rwx  owner=admin  group=banned   secret.txt
```

`banned` グループの人は何もできず、その他の人は全部できる。`banned` に入れられた人が、自分の補助グループから `banned` を**外せたら**、「その他の人」として判定されて読めてしまう。

**user namespace とのつながり**: namespace の中では誰でも全権限を持てるので、放っておくと `setgroups` で `banned` を外せてしまう。
そこで「`sudo` なしで `gid_map` を書くなら、先に `/proc/self/setgroups` に `deny` を書いて、グループを外す道を塞げ」という規則になっている。

- `/proc/self/setgroups` の初期値 `allow` は親の namespace からの引き継ぎ。`allow` のままでは `gid_map` を書けない(EPERM)
- 順番は `setgroups` に `deny` → `gid_map`。`gid_map` を先に書くと、もう `deny` にできない
- 一次資料: `man 7 user_namespaces` の「Defining user and group ID mappings」と「The /proc/pid/setgroups file」

## 分かったこと

<!-- 自分の言葉で。上の「説明の控え」を見ずに書けるかを試すとよい -->

- namespace にはいくつかの種類がある。ステップ1では、User namespace, Pid namespace を学んだ。
- unshare をすると、フラグに応じて新しい namespace が作られる。その後の fork で生成したプロセスは新しい namespace に属することになる。
- unshare を実行したプロセス自身が新しい namespace に属するかどうかは、namespace ごとに異なる。user namespace については、unshare を実行したプロセス自身も新しい User namespace に属するが、Pid namespace は違う。
  - そのプロセスから見えている namespace の一覧は `/proc/self/ns` から確認できる。
- 各プロセスは、自分が属する namespace の一覧のようなものをもっている。自分が属する user namespace はこれ、pid namespace はこれ、といった具合に一覧が配られる。
- User namespace は、プロセスから見える uid, gid を親のものと対応付けることができる。そのプロセス自身は uid=0 と思っているが、親の namespace 側から見ると、uid=1000 だったりする。
  - その対応関係は、`/proc/self/uid_map`, `/proc/self/gid_map` に記録する。
  - 単に unshare, fork しただけではなにもかかれていないので、今回 `myns.rs` では対応関係を自分で書き込んだ。
  - フォーマットは、1行毎に1つの対応関係を記録するもので、 `<内から見た uid/gid> <外から見た uid/gid> <個数>` である。
    - `<個数>` は、uid/gid から何個分を対応させるかを指定する。
  - なお、`/proc/self/gid_map` に書き込むには、`/proc/self/setgroups` を `"deny"` にしておく必要がある。これをすることで、その user namespace にいる全プロセスが `setgroups` syscall を呼ぶことができなくなり、特定のグループから自身を抜けさせるような操作ができなくなり、特定のグループに属しているユーザーに意図しない情報漏洩をさせないようにできる。
    - なぜこのルールがいるのか？
      - `setgroups` は通常、`CAP_SETGID` という権限が必要で、普通のプロセスはそれを持っていない。
      - が、`unshare(CLONE_NEWUSER)` で作った user namespace の中では、作ったプロセス自身が権限を持っており、さらに fork した子プロセスも同じ権限を引き継ぐので、誰でも全権限を持つことになり、`setgroups` を呼べてしまう。
      - よって、上のような制約が必要になる。
- Pid namespace は、プロセスから見える pid を親から見えるものとは別のものに対応させるもの。
  - 親から見れば普通のプロセスだが、子自身は pid=1 など、自分を特殊なプロセスと認識させることができる。

> ✅ (10-25)「namespace は種類ごとに別々」「作られるのは `unshare` の時点だが、本人が入るかは種類で違う」「対応表は自分で書く」
> 「`setgroups` を `deny` にしないと `gid_map` を書けない」。ステップ 1 の要点は全部入っていて、説明の控えを見ずに書けている
>
> ⚠️ `setgroups` の項目を2点だけ:
> (1) `deny` で禁止されるのは「他のプロセス」ではなく、**その user namespace にいる全プロセス(自分も含む)**
> (2) **なぜ user namespace のときだけ**この規則が要るのか(namespace の中では誰でも全権限を持てるので、
> 普段は `CAP_SETGID` が無くて呼べない `setgroups` が呼べてしまう)が抜けている。ここが規則の理由の核心
>
> ✅ (10-25 修正後)(1)(2)とも入った。「普段は `CAP_SETGID` が無い → `unshare(CLONE_NEWUSER)` で作った user namespace の中では持ててしまう → だから塞ぐ」の流れで、規則の理由まで説明できている
