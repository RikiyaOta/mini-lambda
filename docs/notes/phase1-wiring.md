# Phase 1: 縦の配線 + 計測基盤

**日付**: 2026-10-10 開始
**目安**: 3〜5日(ROADMAP: 「さっさと終わらせる」)
**ゴール**: 呼び出し → 実行 → レスポンスが通り、**数字が出る**。バックエンドが差し替え可能になっている

## 分担(2026-10-10 決定: 案 A)

| 部分 | crate | 書く人 |
|---|---|---|
| `manifest.toml` の形、`Function`、`Sandbox` / `Instance` trait | `crates/faas-core` | 作者 |
| 子プロセスで実行、stdout を返す、タイムアウト | `crates/sandbox-process` | 作者 |
| HTTP 層(axum、`POST /invoke/:function_name`) | `crates/faas-api` | エージェント(作者は読んで理解する) |
| ベンチハーネス | `bench/` | エージェント(同上) |

## ステップ

| # | やること | 書く人 | 状態 |
|---|---|---|---|
| 0 | workspace の骨組み(ルートの `Cargo.toml`、空の crate 4つ) | エージェント | ✅ 10-10 |
| 1 | `manifest.toml` の形を決め、`faas-core` に `Function` と読み込み、trait 2つを書く | 作者 | ✅ 10-10 |
| 2 | `sandbox-process` で `Sandbox` を実装(起動、invoke、タイムアウト、destroy) | 作者 | 🔶 10-15 入出力が通った |
| 3 | `faas-api` で HTTP から `Sandbox` を呼ぶ | エージェント | - |
| 4 | `bench/` でコールドスタート等を測り、`results/phase1.csv` に残す | エージェント | - |
| 5 | ADR `docs/decisions/0001-sandbox-trait.md`(なぜこの trait の形にしたか) | 作者 | - |

## メモ

- **ステップ 1(10-10)**: trait の `async fn` は、公開 trait にすると Future に `Send` を約束できないと警告される
  (`async_fn_in_trait`)。axum / tokio のタスクはスレッドをまたぐので Future も `Send` が要る →
  `fn spawn(..) -> impl Future<Output = ..> + Send` の形にした(コンパイラの `help:` が提案した形)。ADR の材料
- `std::fs::write` は途中のディレクトリを作らない → テストで `create_dir_all` が要った
- `Function`(name, base_dir, manifest)と `FunctionManifest`(manifest.toml そのもの)を分けた。`Runtime` は enum + `rename_all = "lowercase"`
- **ステップ 2(10-15)**: 案 1(`spawn` は準備だけ、`invoke` のたびに子プロセスを起動)にした
  - `cat` が止まった原因: payload を**引数**で渡していた + tokio の `output()` は stdin に何もしない → 親(ターミナル)の stdin を引き継いで待ち続けた
  - `Stdio::piped()` = `myrun` の `pipe → fork → dup2` を tokio が代わりにやる。親の手元に残る端が `child.stdin`(書く用)/ `child.stdout`(読む用)
  - **`ChildStdin` を drop = 書き込み側の fd を close = 子に EOF が届く**。閉じないと `cat` は終わらない
  - `spawn()` のときは stdout も自分で `piped()` にする(`output()` は自動でやっていた)
  - `child.stdin.unwrap()` は中身を持ち去る(部分 move)→ `wait_with_output(self)` が呼べない。`take()` で取り出して `None` を残す
  - 次: payload 1 MiB で止まるか(パイプ 64 KiB の罠)、タイムアウトの値をどこから取るか

## 分かったこと
