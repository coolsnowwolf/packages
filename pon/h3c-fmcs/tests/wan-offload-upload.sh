#!/bin/sh
set -e

if [ "$#" -lt 2 ]; then
    echo "usage: $0 ONU_MAC WAN_IP [PORT=49361] [CLIENT_TIMEOUT=30]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP="$2"
PORT="${3:-49361}"
CLIENT_TIMEOUT="${4:-30}"

rm -f "$SAMPLE_LOG" "$CLIENT_OUT"

echo "[*] Step 1: Dispatching ONU upload on $ONU_MAC (16 MiB)..."
cmd="dd if=/dev/zero bs=1048576 count=16 2>/dev/null | nc $WAN_IP $PORT"
fttrctl exec --wait --timeout "$CLIENT_TIMEOUT" --json "$ONU_MAC" "$cmd" > "$CLIENT_OUT" &
EXEC_PID=$!

echo "[*] Step 2: Sampling upload traffic & offload progression..."
s=1
while [ "$s" -le 20 ]; do
    ts=$(date +%T)
    bind_lines=$(cat /sys/kernel/debug/ppe/bind 2>/dev/null | grep "$PORT" || true)
    ct_line=$(grep "dport=$PORT\|sport=$PORT" /proc/net/nf_conntrack 2>/dev/null || true)
    
    printf 'SAMPLE %02d [%s]\n' "$s" "$ts" >> "$SAMPLE_LOG"
    if [ -n "$bind_lines" ]; then
        printf '  PPE_BIND: %s\n' "$bind_lines" >> "$SAMPLE_LOG"
    else
        printf '  PPE_BIND: (none)\n' >> "$SAMPLE_LOG"
    fi
    if [ -n "$ct_line" ]; then
        printf '  CONNTRACK: %s\n' "$ct_line" >> "$SAMPLE_LOG"
    fi

    s=$((s + 1))
    sleep 1
done

echo "[*] Waiting for client exec to complete..."
wait "$EXEC_PID" || true
echo "[+] Client execution completed."
cat "$CLIENT_OUT"
