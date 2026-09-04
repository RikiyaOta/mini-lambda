# AWS Lambda 再実装ロードマップ (Rust)

**目的**: 仮想化技術を理解すること。動くFaaSを作ることは手段であって目的ではない。実用性・完成度より、理解の深さと検証可能な学びを優先する。

---

## 現在地

- **Phase -1(OSの基礎)進行中** — 本は一通り読み流した。`experiments/oslearn/` で `nix` を使った演習中
- **Phase 0 の環境構築は完了** — GCPインスタンス作成、KVM動作確認済み
- 次にやること: `mycp`(システムコールでファイルコピー)の **Step 3: strace で観察** から。
  経緯と残タスクは `docs/notes/phase-minus1-syscall.md` に記録済み

---

## スコープ

- HTTPトリガーで受け取ったリクエストに応じて、隔離環境でユーザーコードを実行し結果を返す、最小のFaaS基盤をRustで自作する
- 単一ノードに限定。複数マシンにまたがるスケジューリング/オーケストレーションは対象外
- 隔離手段は1つに決め打ちせず、`trait Sandbox` で抽象化して複数実装を差し替え可能にし、最後に実測で比較する

### 明示的にやらないこと

- マルチノードのオーケストレーション
- 本格的なパッケージング機構、依存解決、レイヤー
- Wasmの深掘り(コンポーネントモデル、WASI動向)
- 課金・認証・マルチテナントのコントロールプレーン
- Web UI

---

## 実行環境

### 制約

手元のM2 Mac(Apple Silicon = ARM)では `/dev/kvm` が使えない。M2はnested virtualization非対応(Apple Silicon での対応はM3以降)なので、Mac上にLinux VMを立ててもその内側でKVMは使えない。**x86_64のLinuxホストをリモートで使う。**

x86_64を選ぶ理由は学習効率。参考資料(KVM APIドキュメント、kvm-hello-world、LWN記事、OSDev wiki、Firecrackerのドキュメント)の大半がx86_64前提であり、aarch64を選ぶとGDT/ページテーブル/e820の代わりにGIC・PSCI・FDTを最初から自力で調べることになる。

### 構成

| 用途                          | 環境                                                   | 該当Phase          |
| ----------------------------- | ------------------------------------------------------ | ------------------ |
| コード編集・OS基礎の練習      | 手元のM2 Mac                                           | Phase -1, 1 の一部 |
| KVM不要な実装と検証           | Oracle Cloud 無料枠(Ampere A1)                         | Phase 1, 2         |
| ビルド・テスト・KVMを使う作業 | GCP `vmm-dev` (n2-standard-4, Spot, asia-northeast1-a) | Phase 3〜7         |
| 正確なベンチ計測              | ベアメタル(必要になったら1ヶ月だけ)                    | Phase 6, 7         |

**macOSではKVM関連クレートはコンパイルすら通らない。** ゲストカーネルのビルドも含め、Phase 3以降はリモート側で行う。編集はVS Code Remote-SSH で直接リモートにつなぐのが快適(rust-analyzerもリモートで動く)。

### インスタンス作成コマンド(記録)

```bash
gcloud compute instances create vmm-dev \
  --zone=asia-northeast1-a \
  --machine-type=n2-standard-4 \
  --enable-nested-virtualization \
  --provisioning-model=SPOT \
  --instance-termination-action=STOP \
  --image-family=ubuntu-2404-lts-amd64 \
  --image-project=ubuntu-os-cloud \
  --boot-disk-size=30GB \
  --boot-disk-type=pd-balanced
```

作業終了時は必ず停止する。

```bash
gcloud compute instances stop vmm-dev --zone=asia-northeast1-a
```

### 費用

Google AI Pro 特典の月$10 Cloudクレジットを利用(Google Developer Program で有効化済み)。Spotで1日3時間×月20日の運用ならコンピュート$1〜2 + ディスク$2〜3 で、クレジットの範囲に収まる見込み。予算アラート設定済み。

> ⚠️ ネスト環境ではVM exitのコストが増える。Phase 6の計測値は**相対比較にのみ使い**、絶対値が必要になったらベアメタルで取り直す。

---

## 全体像

| Phase | 内容                                      | 目安        | 状態           |
| ----- | ----------------------------------------- | ----------- | -------------- |
| -1    | OSの基礎(前提知識)                        | 2〜3週      | 進行中         |
| 0     | 準備と用語の地図                          | 2〜3日      | 環境構築は完了 |
| 1     | 縦の配線 + 計測基盤                       | 3〜5日      |                |
| 2     | OS-level隔離(対照群)                      | 1週間(厳守) |                |
| 3     | 自作VMM①: KVMで数命令動かす               | 2〜3週      |                |
| 4     | 自作VMM②: Linuxをブートする               | 3〜4週      |                |
| 5     | guest agent と FaaS 統合                  | 1〜2週      |                |
| 6     | コールドスタート最適化 / スナップショット | 2〜3週      |                |
| 7     | Firecracker統合と5バックエンド比較        | 1週間       |                |
| 8     | 発展(任意)                                | —           |                |

フルタイム換算で4ヶ月程度。Phase 3・4・6で全体の6割。

> **中断可能ポイント**: Phase 4を終えた時点で「仮想化技術を学ぶ」目的の7〜8割は達成できる。時間が取れなくなったらそこで一区切りにしてよい。逆にPhase 3を飛ばすとこの計画の意味の大半が失われる。

---

## Phase -1: OSの基礎(2〜3週)【進行中】

KVMを触る前に、ユーザー空間とカーネルの境界を体で理解しておく。ここが曖昧なままPhase 3に入るとほぼ確実に足が止まる。

- [x] 『ふつうのLinuxプログラミング』を入手して読み始める
- [ ] 読み終える。**読み終えてから始めるのではなく、読みながら手を動かす**
- [ ] 本のサンプルはCだが、**Rustと`nix`クレートで書き直しながら読む**。CとRustの差分が理解のフックになる
- [ ] `strace` で自分のプログラムが発行したシステムコールを観察する習慣をつける
- [ ] 押さえる概念:
    - [x] ファイルディスクリプタ(カーネルが実体を持ち、ユーザー空間には番号だけが渡る)
    - [x] `read`/`write` が要求より少なく返ること、`EINTR` の再試行 — `mycp` で実装(`docs/notes/phase-minus1-syscall.md`)
    - [ ] システムコールの往復コスト(バッファサイズを1バイトにして体感する)
    - [ ] プロセスとfork/exec、シグナル、パイプ
    - [ ] 仮想メモリ(こちらも「仮想化」と呼ばれるが別概念。ただしEPTの理解に直結する)

### 到達点の確認

- [ ] `std::fs::copy` を使わずにファイルコピーを書き、`strace` で発行されたシステムコールを説明できる
- [ ] 「なぜアプリはディスクを直接触れないのか」を自分の言葉で説明できる
- [ ] `BufReader` が存在する理由を、システムコールのコストの観点で説明できる

> 7割理解できたら次へ進む。残りはPhase 3で詰まったときに戻ってくれば入る。

---

## Phase 0: 準備と用語の地図(2〜3日)

### 環境【完了】

- [x] GCPインスタンス作成(nested virtualization有効、Intel系、Spot)
- [x] SSH接続確認
- [x] `/dev/kvm` の存在確認、`kvm-ok` で `KVM acceleration can be used` を確認
- [x] `usermod -aG kvm` で権限設定
- [x] QEMUでホストカーネルを起動し、KVMが実際に動作することを確認(rootfs無しのpanicまで到達)
- [x] 予算アラート設定
- [x] Google AI Pro の Cloudクレジット有効化

### リポジトリ

- [ ] リポジトリ作成、名前を決める
- [ ] ディレクトリ構成(下記「リポジトリ構成」参照)
- [ ] `CLAUDE.md` / `AGENTS.md` を配置
- [ ] CI(後述)
- [ ] README

### 用語の地図

`docs/glossary.md` に育てていく。

- [ ] ハードウェア仮想化 / OS-level仮想化(コンテナ) / 言語レベルサンドボックス の3層の違い
- [ ] CPUのモード: 特権モード / 非特権モード / **ゲストモード**(VT-x, AMD-V)
- [ ] VM entry と VM exit、trap-and-emulate
- [ ] EPT(2段階アドレス変換)、guest physical / host virtual の対応関係
- [ ] 完全仮想化 と 準仮想化(virtio)
- [ ] **KVM(カーネル側:CPUとメモリ)と VMM(ユーザ空間:デバイス)の責務分担** — ここが曖昧だとPhase 3で必ず詰まる
- [ ] vCPU が「ゲストのCPU役を務めている時間帯のホストのスレッド」であること

### 関数フォーマットの設計

- [ ] `manifest.toml`(handler, runtime, timeout_ms, memory_mib, env)+ コード本体
- [ ] 最初は単一の実行ファイル/スクリプトのみ対応。パッケージング機構に凝らない

**ゴール**: 環境が整い、上の用語を自分の言葉で説明できる

---

## Phase 1: 縦の配線 + 計測基盤(3〜5日)

ここは**さっさと終わらせる**。凝るとPhase 3に到達する前に力尽きる。

- [ ] axum で `POST /invoke/:function_name`
- [ ] `std::process::Command` でサブプロセス実行、stdout をレスポンスに
- [ ] タイムアウト処理
- [ ] **隔離バックエンドのtraitを最初に切る**:

```rust
#[async_trait]
trait Sandbox: Send + Sync {
    type Instance: Instance;
    /// インスタンスを起動し、呼び出し可能になるまで待つ
    async fn spawn(&self, f: &Function) -> Result<Self::Instance>;
}

#[async_trait]
trait Instance: Send {
    async fn invoke(&mut self, payload: &[u8]) -> Result<Vec<u8>>;
    async fn destroy(self) -> Result<()>;
    fn metrics(&self) -> InstanceMetrics;  // 起動内訳、RSS など
}
```

### ベンチハーネス

`bench/` を作り、以降すべてのPhaseで同じものを流す。**これが学習のフィードバックループになる。**

- [ ] コールドスタート(起動要求 → 最初のレスポンスバイト)p50 / p99
- [ ] **コールドスタートの内訳**(段階ごとにタイムスタンプを打つ。Phase 6の最適化対象)
- [ ] ウォーム呼び出しレイテンシ p50 / p99
- [ ] インスタンスあたりのメモリフットプリント(ホストから見たRSS)
- [ ] 単一ホストでの同時起動可能数
- [ ] teardown時間
- [ ] 結果を `results/phaseN.csv` に残し、READMEに表として追記していく

**ゴール**: 呼び出し→実行→レスポンスが通り、**数字が出る**。バックエンドが差し替え可能になっている

---

## Phase 2: OS-level隔離(1週間・厳守)

深追いしない。目的は**「共有カーネル型の隔離」を体で知り、後でVMと比較できるようにすること**。これがDockerの仕組みでもある。

- [ ] `nix` クレートで `clone()` / `unshare()` によるnamespace分離(PID / mount / net / user / uts / ipc)
- [ ] `pivot_root` で最小rootfsに切り替え
- [ ] cgroup v2: `memory.max`, `cpu.max`, `pids.max` を直接ファイルに書く
- [ ] `libseccomp-rs` でseccomp-bpfフィルタ。**まず全部deny → 動くまで許可を足す**方式で、必要なsyscall数を数える
- [ ] `Sandbox` 実装として登録し、ベンチを流す

### 答えられるようになるべき問い

- 許可が必要だったsyscallは何個だったか。**それがそのまま攻撃面のサイズ**である
- なぜAWSはLambdaをコンテナではなくmicroVMで動かすことにしたのか(Firecrackerの論文の論拠を自分の実測と照らす)
- seccompで塞げるもの / 塞げないもの(カーネルのバグ、サイドチャネル、投機実行系)
- user namespaceがあってもrootless化で残るリスクは何か

**ゴール**: nsjail的な最小サンドボックスが動き、ベンチの数字が1行増えている

---

## Phase 3: 自作VMM① — KVMで数命令動かす(2〜3週)

**ここが本命の入り口。いきなりLinuxを起動しようとしない。** まず数バイトのアセンブリを動かす。

### 3-1. 生ioctlで最小ループ(一度は自分で書く)

- [ ] `/dev/kvm` を open → `KVM_GET_API_VERSION`
- [ ] `KVM_CREATE_VM` → VM fd を得る
- [ ] ホスト側で `mmap` した匿名メモリを `KVM_SET_USER_MEMORY_REGION` でゲスト物理メモリとして登録
- [ ] `KVM_CREATE_VCPU` → vCPU fd、`KVM_GET_VCPU_MMAP_SIZE` して `kvm_run` 構造体を mmap
- [ ] `KVM_SET_REGS` / `KVM_SET_SREGS` でリアルモードの初期状態を作る
- [ ] ゲストメモリに手書きの機械語を配置(`mov`, `out`, `hlt` の数バイト)
- [ ] `KVM_RUN` ループを書き、`kvm_run.exit_reason` で分岐。`KVM_EXIT_IO` を捕まえてホスト側で文字を出力する

VMMの心臓部は本質的にこれだけ:

```rust
loop {
    vcpu.run()?;                       // ゲストを走らせる。VM exit で戻る
    match vcpu.kvm_run().exit_reason {
        KVM_EXIT_IO   => { /* ゲストがI/Oポートを触った */ }
        KVM_EXIT_MMIO => { /* ゲストがデバイスのメモリ領域を触った */ }
        KVM_EXIT_HLT  => break,
        r => panic!("unhandled exit: {r}"),
    }
}
```

### 3-2. long modeへ

- [ ] GDTをゲストメモリに配置し、`KVM_SET_SREGS` で CR0 / CR4 / EFER を設定
- [ ] identity mapのページテーブル(PML4 / PDPT / PD)をホストが構築してCR3に設定
- [ ] 64bitコードが動くことを確認
- [ ] `KVM_SET_CPUID2` でCPUIDをフィルタ(hypervisorビット、最大leafの制限)

### 3-3. rust-vmm クレートへの移行

- [ ] `kvm-ioctls` / `kvm-bindings` / `vm-memory` / `vmm-sys-util` に置き換える
- [ ] 生ioctl版と読み比べ、各クレートが何を抽象化しているかを把握する

### 答えられるようになるべき問い

- `KVM_RUN` から戻ってくるのは具体的にどんな時か。exit_reasonを5種類以上挙げられるか
- MMIOとPIOの扱いの違い。なぜMMIOの方がVM exitのコストで問題になるのか
- EPTがなかった時代(shadow page table)と比べて何が速くなったのか
- ゲスト物理アドレス → ホスト仮想アドレス の対応は誰が持っているのか
- KVMに任せている仕事と、ユーザ空間でやっている仕事の境界線はどこか

**ゴール**: 自作VMMが64bitモードで任意の機械語を実行し、I/Oでホストと通信できる

---

## Phase 4: 自作VMM② — Linuxをブートする(3〜4週)

**このフェーズのご褒美は「自分が書いたVMMのstdoutにゲストのdmesgが流れ始める瞬間」。** Phase 0 でQEMUを使って見たあの光景を、自分のコードで再現する。

### 4-1. カーネルのロードとブート

- [ ] ゲストカーネルを自分でビルドする(最小configから。ここでの作業がPhase 6の下地になる)
- [ ] `linux-loader` クレートでロード。**まずELF(`vmlinux`)、慣れたらbzImage**
- [ ] `boot_params`(zero page)を構築: e820メモリマップ、cmdlineアドレス、ramdisk情報
- [ ] MPTable(またはACPIテーブル)を配置してCPU構成をゲストに伝える
- [ ] `KVM_CREATE_IRQCHIP` でPIC / IOAPIC / LAPIC をカーネル内蔵にする(自前実装は学習コスパが悪い)、`KVM_CREATE_PIT2`

### 4-2. 最小デバイス

- [ ] **8250/16550 UART(0x3f8)** — `vm-superio` の `Serial` を使うか自作。**これが出た時点で勝ち**
- [ ] i8042 スタブ(ゲストからのリセット検知に必要)
- [ ] initramfs(busybox + 最小init)でシェルまで到達させる

### 4-3. virtio デバイス層

**virtioの理解はこのプロジェクト最大の実用的リターン。** コンテナもVMもクラウドも、LinuxのI/Oは今ほぼ全部この上に乗っている。

- [ ] virtio-mmio のトランスポート層を実装(デバイスツリー不要。Firecracker同様、cmdlineに `virtio_mmio.device=4K@0xd0000000:5` 形式で渡す)
- [ ] virtqueue: descriptor table / available ring / used ring を自分で読み書きする
- [ ] 通知経路: ゲストのMMIO write → VM exit → ホストで処理
- [ ] **次に `ioeventfd` / `irqfd` に置き換えてVM exitを削る**。前後でレイテンシを測ると効果が数字で見える
- [ ] **virtio-blk**: ext4イメージをrootfsとしてマウント(`dd` → `mkfs.ext4` → alpine minirootfs展開 で作る)
- [ ] **virtio-net + tap**: ゲストにIPを振り、ホストとping疎通
- [ ] **virtio-vsock**(まずvirtio-consoleでもよい): ゲストエージェントとの制御チャネル

### 答えられるようになるべき問い

- virtqueueで1回のI/Oあたり何回VM exitが起きるか。ioeventfd/irqfdはそれをどう減らすか
- なぜvirtioを準仮想化と呼ぶのか。エミュレートされたNICと比べて何を捨てて何を得たか
- **システムコールの往復コストとVM exitのコストは、構造的に何が同じで何が違うか**(Phase -1 の `BufReader` の話と接続する)
- カーネルのブート時間の内訳はどこが支配的か(`initcall_debug` で観測)
- PCIではなくvirtio-mmioを選ぶと何が楽になり、何を諦めるか

**ゴール**: 自作VMM上でLinuxが起動し、シェルが出て、rootfsが読め、ホストとネットワーク疎通できる

---

## Phase 5: guest agent と FaaS 統合(1〜2週)

ここでようやく「Lambdaの再実装」らしい形になる。

- [ ] ゲスト内 init(PID 1)をRustで書く。**vsockで待ち受け → 関数ペイロード受信 → 実行 → 結果を返す**
- [ ] AWS Lambda の Runtime Interface(`/runtime/invocation/next` をポーリング → `/runtime/invocation/{id}/response` に返す)を模して設計する。**なぜ「ゲストが取りに来る」形なのかを考える**
- [ ] rootfsのビルドを自動化。read-only rootfs + per-VM の書き込み層(overlay)
- [ ] ホスト側: 自作VMMバックエンドを `Sandbox` として登録
- [ ] tokioで非同期化。VM起動・vsock通信・タイムアウトを並行に扱う
- [ ] ウォームインスタンスのプール + キュー&ワーカーモデルで同時リクエストを捌く
- [ ] ベンチを流す。**この時点でのコールドスタートはおそらく数百ms〜1秒**

**ゴール**: HTTPリクエストが自作microVM内のユーザーコードで処理され、結果が返る

---

## Phase 6: コールドスタート最適化とスナップショット(2〜3週)

**Lambdaという製品の技術的な本質はここにある。** かつ仮想化の理解が一番深まる。

### 6-1. まず内訳を測る

- [ ] VMMプロセス起動 / KVMセットアップ / カーネルブート / init / agent ready / 関数実行 に分解
- [ ] カーネル側は `initcall_debug` + TSC で内訳を取る

### 6-2. ブート時間の削減

- [ ] カーネルconfigを削る(不要なドライバ、デバッグ機能、モジュールサポート)
- [ ] cmdline: `quiet`, `reboot=k`, `panic=1`、不要なサブシステムの無効化
- [ ] initを最小化(busybox経由をやめて自作agentを直接PID 1に)
- [ ] **削る前後で必ず計測**。効いた項目と効かなかった項目を記録する

### 6-3. スナップショット / リストア(最重要)

- [ ] **保存**: vCPU状態(`KVM_GET_REGS` / `SREGS` / `MSRS` / `XSAVE` / `LAPIC` / `VCPU_EVENTS` / `MP_STATE`)、デバイス状態(virtqueueのインデックス、UARTのレジスタ)、ゲストメモリ全体、KVM clock / TSC offset
- [ ] **復元**: 対応する `KVM_SET_*`、メモリはファイルからmmap
- [ ] **高速化①**: メモリファイルを `MAP_PRIVATE` でマップし、CoWで複数VMが同一スナップショットを共有
- [ ] **高速化②**: `userfaultfd` による遅延ページング。**Firecrackerがコールドスタートを劇的に縮めている仕組みの核**
- [ ] 復元時間 vs 通常ブートを比較。**桁が違うはず**

### 6-4. クローンの落とし穴(セキュリティ的に重要)

同一スナップショットから多重復元すると壊れるもの:

- [ ] エントロピープール / 乱数(→ 復元後に再seed。VMGENID相当の仕組み)
- [ ] 時刻のずれ(→ 復元後のclock補正)
- [ ] MACアドレス / IPの重複
- [ ] 復元前に確立したTLSセッション鍵やnonceの再利用
- [ ] 「なぜFirecrackerのドキュメントがスナップショットの多重復元に警告を出しているのか」を自分の言葉で説明できるようにする

**ゴール**: スナップショットからの復元でコールドスタートが桁で改善し、その理由と危険性を説明できる

---

## Phase 7: Firecracker統合と5バックエンド比較(1週間)

**自作VMMを通した後だからこそ、Firecrackerを読むのが速い。**

- [ ] Firecrackerバックエンドを `Sandbox` 実装として追加(Unixソケット越しのREST API、`jailer` 経由の権限分離)
- [ ] Wasmtimeバックエンドを**1〜2日で**追加。深入りしない
- [ ] 5つのバックエンドで同じベンチを流し、表を埋める:

|                       | 生プロセス | namespace+cgroup | Wasmtime | 自作VMM | Firecracker |
| --------------------- | ---------- | ---------------- | -------- | ------- | ----------- |
| コールドスタート p50  |            |                  |          |         |             |
| ウォーム呼び出し p50  |            |                  |          |         |             |
| メモリ/インスタンス   |            |                  |          |         |             |
| 同時実行数(上限)      |            |                  |          |         |             |
| 攻撃面(カーネル共有?) |            |                  |          |         |             |
| 対応言語の制約        |            |                  |          |         |             |

- [ ] **自作VMMがFirecrackerに負けている項目について、ソースを読んで理由を特定する**。ここが最も学びの濃い作業になる
- [ ] Cloud Hypervisor / crosvm と設計を比較

---

## Phase 8: 発展(任意)

- [ ] `vhost-user` でデバイスをVMM外プロセスに切り出す
- [ ] io_uring によるI/Oパス最適化
- [ ] virtio-fs でホスト↔ゲストのファイル共有
- [ ] balloon / free page reporting によるメモリオーバーコミット
- [ ] VMM自身へのseccomp適用、jailer相当の権限分離
- [ ] マルチテナントのネットワーク(tap + nftables、あるいはpasta)
- [ ] per-tenantリソースクォータ、ログ/メトリクス収集
- [ ] サイドチャネル対策の調査(L1TF、MDS、CPUピニングとSMT無効化のトレードオフ)

---

## リポジトリ構成

```
.
├── CLAUDE.md              # AIエージェントへの指示(最重要ルールを含む)
├── AGENTS.md              # CLAUDE.md へのシンボリックリンク
├── README.md              # 1行目にプロジェクトの目的
├── ROADMAP.md             # このファイル
├── docs/
│   ├── glossary.md        # 用語集。初出の用語は必ずここに追加
│   ├── environment.md     # 環境構築手順と再現コマンド
│   ├── decisions/         # 設計判断とその理由(ADR)
│   │   └── 0001-sandbox-trait.md
│   └── notes/             # Phaseごとの作業記録・つまずき・学び
│       └── phase3-kvm.md
├── results/               # ベンチマークの生データ(CSV)
├── experiments/           # 学習用の使い捨てコード(本体とは切り離す)
│   └── oslearn/           # Phase -1: nix でsyscallを直接叩く練習(src/bin/ に足していく)
├── crates/
│   ├── faas-api/          # HTTP層(axum)
│   ├── faas-core/         # trait Sandbox、Function、manifest
│   ├── sandbox-process/   # Phase 1: 素のサブプロセス
│   ├── sandbox-ns/        # Phase 2: namespace + cgroup + seccomp
│   ├── vmm/               # Phase 3-4: 自作VMM ★中核
│   ├── sandbox-vmm/       # Phase 5: 自作VMMをSandboxとして繋ぐ
│   ├── sandbox-firecracker/ # Phase 7
│   ├── sandbox-wasm/      # Phase 7
│   └── guest-init/        # ゲスト内で動くPID 1
├── bench/                 # ベンチハーネス
├── guest/                 # カーネルconfig、rootfsビルドスクリプト
└── .github/workflows/ci.yml
```

`crates/vmm/` には、そのディレクトリ固有のルールを書いた `CLAUDE.md` を別途置く(「ここのコードはエージェントが書かない」を再宣言する)。

---

## CI (GitHub Actions)

GitHub-hosted の **x86 Linux ランナーには `/dev/kvm` がある**(Androidエミュレータ向けに開放されたもの)。権限設定が必要。

```yaml
jobs:
    vmm-test:
        runs-on: ubuntu-24.04 # arm ランナーは不可
        steps:
            - uses: actions/checkout@v4
            - name: Enable KVM
              run: |
                  echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
                    | sudo tee /etc/udev/rules.d/99-kvm4all.rules
                  sudo udevadm control --reload-rules
                  sudo udevadm trigger --name-match=kvm
            - name: Preflight
              run: test -e /dev/kvm || { echo "KVM unavailable"; exit 1; }
            - run: cargo test --test boot_smoke
```

- **回す**: カーネル起動のスモークテスト、vsock経由のinvoke、Phase 2のnamespace/cgroup/seccompテスト
- **回さない**: **ベンチマーク**。ネスト環境 + 共有CPUなので数字が意味を成さない
- `/dev/kvm` の提供は公式にドキュメント化されていないため、preflightで明示的に落とす
- ゲストカーネルとrootfsは毎回ビルドせず、artifactかGH Cacheに置いて再利用する

---

## 参考資料

### 一次資料(最優先)

- Linuxカーネルの `Documentation/virt/kvm/api.rst` — **KVMのioctlは全部ここに書いてある**。Phase 3〜4の主教材
- virtio spec(OASIS, v1.2以降)— virtqueueの章
- Linux boot protocol(`Documentation/arch/x86/boot.rst`)

### コード

- `kvm-hello-world`(dpw/kvm-hello-world)— C。KVM最小例として最短
- rust-vmm の各クレート: `kvm-ioctls`, `kvm-bindings`, `vm-memory`, `linux-loader`, `vm-superio`, `virtio-queue`, `event-manager`, `vmm-sys-util`。`rust-vmm/rust-vmm` モノレポへの集約が進行中
- `rust-vmm/vmm-reference` — 最小VMMの実装例。**2026年5月にアーカイブされread-onlyになった**ので、動く最新実装としてではなく読み物として使う
- Firecracker(`src/vmm/src/`)— Phase 4以降、詰まった箇所をピンポイントで読む
- Cloud Hypervisor / crosvm — 比較対象。crosvmはデバイス実装が読みやすい

### 論文・記事・書籍

- 『ふつうのLinuxプログラミング』— Phase -1 の教材
- LWN の "Using the KVM API"(Josh Triplett)
- Firecracker: Lightweight Virtualization for Serverless Applications(NSDI '20)
- Ilúvatar、Rik(Polyxia)— FaaSスケジューリング寄りの参考実装

---

## 運用ルール

1. **「動かす → 測る → 読む」の順を守る**。先にFirecrackerのソースを読み始めると必ず迷子になる
2. **Phaseごとに、数字と短い技術メモを残す**。書けないなら理解できていない
3. **タイムボックスを守る**。特にPhase 2と、Phase 7のWasm
4. **中核(3・4・6)では横道に逸れてよい**。ここでの寄り道は全部が本題
5. **7割理解できたら次へ進む**
6. **作業終了時にGCPインスタンスを停止する**
