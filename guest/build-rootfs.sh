#!/usr/bin/env bash
# Phase 2 用の最小 rootfs を作る。中身は静的リンクの busybox 1つと、そのコマンド名へのシンボリックリンクだけ。
#
#   ./guest/build-rootfs.sh            # → guest/out/rootfs/ にできる(既にあれば作り直す)
#   ./guest/build-rootfs.sh <出力先>
#
# 前提: busybox-static(`sudo apt install busybox-static`)。root 権限は不要。
# エージェントが書いた部分(CLAUDE.md の「rootfs イメージのビルドスクリプト」)。
set -euo pipefail

out="${1:-$(dirname "$0")/out/rootfs}"
busybox="$(command -v busybox || true)"

if [[ -z "$busybox" ]]; then
  echo "busybox が見つかりません。sudo apt install busybox-static で入れてください" >&2
  exit 1
fi
# 動的リンクの busybox だと、rootfs の中に libc などが無いので動かない
if ! file -L "$busybox" | grep -q "statically linked"; then
  echo "$busybox は静的リンクではありません(busybox-static が必要)" >&2
  exit 1
fi

rm -rf "$out"
mkdir -p "$out"/{bin,proc,dev,tmp,etc,root}
chmod 1777 "$out/tmp"

cp "$busybox" "$out/bin/busybox"
# busybox は「どの名前で呼ばれたか(argv[0])」で動作を切り替える。sh → busybox などのリンクを並べる
for cmd in $("$out/bin/busybox" --list); do
  [[ "$cmd" == busybox ]] && continue
  ln -s busybox "$out/bin/$cmd"
done

# 中で uid / gid 0 に名前が付くように(無くても動く。ps や id の表示が数字になるだけ)
echo 'root:x:0:0:root:/root:/bin/sh' > "$out/etc/passwd"
echo 'root:x:0:'                    > "$out/etc/group"

echo "rootfs: $out ($(ls "$out/bin" | wc -l) commands, $(du -sh "$out" | cut -f1))"
