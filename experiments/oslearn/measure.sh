#!/usr/bin/env bash
# バッファサイズを変えながら mycp を実行し、実時間とsyscall回数を results/ に残す。
#
# 使い方: ./measure.sh [コピー元ファイル]
#   コピー元を省略した場合は /tmp/src.bin を 10MiB で作る。
set -euo pipefail

cd "$(dirname "$0")"

SRC=${1:-/tmp/src.bin}
DST=/tmp/mycp-bench.dst
BIN=./target/release/mycp
REPEAT=3
SIZES="1 16 512 4096 8192 65536 1048576"
OUT=../../results/phase-minus1-bufsize.csv

if [ ! -f "$SRC" ]; then
    echo "コピー元が無いので作成: $SRC (10MiB)"
    head -c 10M /dev/urandom >"$SRC"
fi

cargo build --release -q
mkdir -p "$(dirname "$OUT")"

FILE_BYTES=$(stat -c %s "$SRC")
echo "コピー元: $SRC ($FILE_BYTES バイト) / 各サイズ ${REPEAT}回測って中央値"
echo

printf "%10s %12s %12s %10s %10s %10s %12s\n" \
    "buf_size" "read呼出" "write呼出" "real(s)" "user(s)" "sys(s)" "MiB/s"

echo "buf_size,file_bytes,read_calls,write_calls,real_sec,user_sec,sys_sec,throughput_mib_s" >"$OUT"

for size in $SIZES; do
    # syscall回数は計算で出す(1バイトバッファでstraceを噛ませると桁違いに遅くなるため)。
    # read は最後にEOF(0)を確認する1回が余分に走る。
    read_calls=$(((FILE_BYTES + size - 1) / size + 1))
    write_calls=$(((FILE_BYTES + size - 1) / size))

    reals=()
    users=()
    syss=()
    for _ in $(seq "$REPEAT"); do
        TIMEFORMAT='%R %U %S'
        # time の出力(stderr)だけを拾う。本体の出力は捨てる。
        t=$( { time "$BIN" "$SRC" "$DST" "$size" >/dev/null 2>/dev/null; } 2>&1 )
        reals+=("$(echo "$t" | awk '{print $1}')")
        users+=("$(echo "$t" | awk '{print $2}')")
        syss+=("$(echo "$t" | awk '{print $3}')")
    done

    # 3回の中央値を取る
    median() { printf '%s\n' "$@" | sort -g | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
    real=$(median "${reals[@]}")
    user=$(median "${users[@]}")
    sys=$(median "${syss[@]}")

    mibs=$(awk -v b="$FILE_BYTES" -v s="$real" 'BEGIN{ printf "%.1f", (s>0) ? b/1048576/s : 0 }')

    printf "%10d %12d %12d %10s %10s %10s %12s\n" \
        "$size" "$read_calls" "$write_calls" "$real" "$user" "$sys" "$mibs"
    echo "$size,$FILE_BYTES,$read_calls,$write_calls,$real,$user,$sys,$mibs" >>"$OUT"
done

rm -f "$DST"
echo
echo "書き出し: $OUT"
