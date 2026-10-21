# 0001: 隔離バックエンドを `Sandbox` / `Instance` trait で抽象化する

- 日付: 2026-10-21
- 状態: 採用

## 背景

<!-- なぜ trait が必要だったか。このプロジェクトで隔離方式をいくつ作る予定か(ROADMAP の 5 バックエンド)、最後に何をしたいか -->

- このプロジェクトでは複数の隔離方式を実装する予定だったため、実行環境の生成・実行を共通のインターフェースで行えるよう、trait を定義するようにした。
- このプロジェクトでは5つの隔離方式を実装する予定。
- 最後に、それらの隔離方式を、パフォーマンスや安全性、制約などの観点で比較したい。

## 決めたこと

<!-- trait の形を貼る(crates/faas-core/src/lib.rs から)。Sandbox と Instance に分けた理由を 1〜2 行 -->

`trait` の形を以下のように決めた：

```rust
trait Sandbox: Send + Sync {
    type Instance: Instance;

    fn spawn(&self, f: &Function) -> impl Future<Output = Result<Self::Instance>> + Send;
}
```

```rust
trait Instance: Send {
    /// 実行環境が作られた関数を呼び出す.
    fn invoke(&mut self, payload: &[u8]) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// 実行環境を破棄する.
    fn destroy(self) -> impl Future<Output = Result<()>> + Send;

    /// 実行環境の中身を観察する.
    fn metrics(&self) -> InstanceMetrics;
}
```

- `Sandbox` が、隔離環境をどのように作るかを決めるもの。
- `Instance` が、`Sandbox` によって実際に作られた実行環境。
- 1つの隔離方式で複数の実行環境を作ることが想定されるので、この2つをわける実装にした。

### `spawn` と `invoke` の役割分担

<!-- sandbox-process では spawn は準備だけ、invoke ごとに子プロセスを起動した。なぜか。VM のバックエンドではどうなりそうか -->

- `sandbox-process` では、`spawn` は準備だけをして、`invoke` で実際に子プロセスを起動した。
- やりたいことがシンプルだったので、単にそうした。
- VM のバックエンドを今後実装していく場合には、VM のインスタンスを `spawn` で作成（つまり、ホストマシンで VM 用の領域を確保することになると思われる）し、`invoke` で実際に VM を起動する、つまり、ホストマシン上でVM用のプロセスを起動することになると予想している。

### `async fn` ではなく `impl Future + Send` にした

<!-- コンパイラの警告(async_fn_in_trait)、なぜ Send が要るのか(faas-api の axum とつなげて) -->

- `async fn` は `fn ... -> impl Future` の省略形である。
- 今回採用したものは `fn ... -> impl Future + Send` であるので、まず定義自体が異なる。`Send` を追加で要求するもの。
- `tokio` では、`Future` をスレッド間で移動させる（`.await` の際にランタイムがタスクを別のワーカースレッドに移すことがある）ため、`Send` を実装している必要がある。
  - 補足：`Send`, `Sync` の違い
    - `Send`: 値そのものを別スレッドに渡せるもの。
    - `Sync`: 参照を複数のスレッドから同時に使って良いもの。
    - なので、`T: Sync` ならば `&T: Send` であるし、逆もまた正しい。
- `faas-api` では、具体的な実装ではなく `Sandbox` を受け取るので、`trait` 側で `Send` を実装することを約束させる必要がある。
- よって、今回は `impl Future + Send` にした。

> ✅ (10-21)「tokio は Future をスレッド間で移す」→「faas-api は trait 越しにしか見えない」→「だから trait 側で約束させる」が一本につながった

## 検討して捨てた案

<!-- 例: spawn で子プロセスを起動しておく案 / async_trait クレートを使う案 / trait を作らずに直接呼ぶ案。1つでよい -->

- `sandbox-process` の `spawn` にて、子プロセスを先に生成しておく案
  - 先にプロセスを生成しておくことにすると、そのプロセスに対して命令を出したり、そのプロセスの生死を確認するなどの複雑さが出てくる。

## 影響

<!-- この形にしたことで良くなったこと、後で困りそうなこと(例: InstanceMetrics が空のまま、destroy が sandbox-process では何もしない) -->

- 良くなったこと
  - `faas-api` 側で、`Sandbox` の具体的な詳細を知らずにコードを書くことができた。
    - 複数の隔離方針を同じコードで動かすことができる見込みとなった。
  - ベンチマークを取るときにも、stages モードによって HTTP を通さずに段階ごとの時間を測ることができた。
    - これは、HTTP 層である `faas-api` から分けて実装したことによる。
    - そのおかげで、`bench` から直接呼ぶことができた。
- 後で困りそうなこと
  - `InstanceMetrics` の詳細な実装は後回しにした。必要な項目が見えてきたら中身を実装していく。
  - `InstanceProcess::destroy` では、なんの処理もしていない。VM だったなら、ホスト側に用意した仮想マシン用の領域やプロセスを破棄するなどの処理が必要と思われるので、そこは追って学習する必要がある。
