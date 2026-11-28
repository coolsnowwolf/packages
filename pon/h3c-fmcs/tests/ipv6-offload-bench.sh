#!/bin/sh
set -e

if [ "$#" -lt 2 ]; then
    echo "usage: $0 ONU_MAC WAN_IP6 [PORT=50002] [BYTES=35000000]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP6="$2"
PORT="${3:-50002}"
BYTES="${4:-35000000}"
LOGFILE="/tmp/ipv6_bench_samples.log"

rm -f "$LOGFILE"

echo "[*] Step 1: Dispatching IPv6 TCP stream from $ONU_MAC to $WAN_IP6:$PORT ($BYTES bytes)..."
fttrctl exec --wait --timeout 25 "$ONU_MAC" "head -c $BYTES /dev/zero | /bin/busybox nc -n -p 50004 $WAN_IP6 $PORT > /dev/null 2>&1" > /dev/null 2>&1 &
CLIENT_PID=$!

echo "[*] Step 2: Sampling PPE bind entries and conntrack progression for IPv6..."
i=1
while [ "$i" -le 15 ]; do
    ts=$(date +%T)
    bind_line=$(cat /sys/kernel/debug/ppe/bind 2>/dev/null | grep "$PORT" || true)
    ct_line=$(grep "$PORT" /proc/net/nf_conntrack 2>/dev/null || true)
    
    printf '[%02d %s]\n' "$i" "$ts" >> "$LOGFILE"
    if [ -n "$bind_line" ]; then
        echo "  PPE_BIND: $bind_line" >> "$LOGFILE"
    else
        echo "  PPE_BIND: (none)" >> "$LOGFILE"
    fi
    if [ -n "$ct_line" ]; then
        echo "  CT: $ct_line" >> "$LOGFILE"
    fi
    
    i=$(( i + 1 ))
    sleep 1
done

wait "$CLIENT_PID" || true
echo "[+] IPv6 benchmark sampling finished."
cat "$LOGFILE"
