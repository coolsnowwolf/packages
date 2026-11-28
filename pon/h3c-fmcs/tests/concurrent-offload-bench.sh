#!/bin/sh
set -e

if [ "$#" -lt 3 ]; then
    echo "usage: $0 ONU_MAC WAN_IP FS_IP [WAN_PORT=50010] [BYTES=30000000]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP="$2"
FS_IP="$3"
WAN_PORT="${4:-50010}"
BYTES="${5:-30000000}"
LOGFILE="/tmp/concurrent_dual_stream.log"

rm -f "$LOGFILE"

echo "[*] Step 1: Deploying concurrent worker on $ONU_MAC..."
# Base64 worker runs two tasks concurrently:
# Task A: L2 UDP stream to peer ONU for 6 seconds
# Task B: L3 TCP stream of $BYTES to WAN receiver
B64_WORKER="IyEvYmluL3NoCkZTX0lQPSIkMSIKV0FOX0lQPSIkMiIKV0FOX1BPUlQ9IiQzIgpCWVRFUz0iJDQiCgooCiAgICBlbmQ9JCgoICQoZGF0ZSArJXMpICsgNiApKQogICAgd2hpbGUgWyAkKGRhdGUgKyVzKSAtbHQgJGVuZCBdOyBkbwogICAgICAgIGRkIGlmPS9kZXYvemVybyBicz0xMDI0IGNvdW50PTEwMCAyPi9kZXYvbnVsbAogICAgICAgIHNsZWVwIDAuMDUKICAgIGRvbmUgfCAvYmluL2J1c3lib3ggbmMgLW4gLXUgLXAgNTAwMjAgJEZTX0lQIDUwMDIwID4gL2Rldi9udWxsIDI+JjEKKSAmCkwyX1BJRD0kIQoKaGVhZCAtYyAkQllURVMgL2Rldi96ZXJvIHwgL2Jpbi9idXN5Ym94IG5jIC1uIC1wIDUwMDEyICRXQU5fSVAgJFdBTl9QT1JUID4gL2Rldi9udWxsIDI+JjEKTDNfUkVUPSQ/Cgp3YWl0ICRMMl9QSUQgfHwgdHJ1ZQplY2hvICJbUkVTVUxUXSBMM19FWElUPSRMM19SRVQi"

fttrctl exec --wait --timeout 10 "$ONU_MAC" "echo '$B64_WORKER' | /bin/base64 -d > /tmp/run_dual.sh; chmod +x /tmp/run_dual.sh"

echo "[*] Step 2: Launching concurrent dual streams (L2 Bridge + L3 WAN)..."
fttrctl exec --wait --timeout 25 "$ONU_MAC" "/tmp/run_dual.sh $FS_IP $WAN_IP $WAN_PORT $BYTES" > /tmp/dual_client.log 2>&1 &
CLIENT_PID=$!

echo "[*] Step 3: Sampling PPE table entries during concurrent traffic..."
i=1
while [ "$i" -le 10 ]; do
    ts=$(date +%T)
    bind_entries=$(cat /sys/kernel/debug/ppe/bind 2>/dev/null || true)
    all_l2b=$(cat /sys/kernel/debug/ppe/entries 2>/dev/null | grep 'L2B' | grep -v 'ff:ff' | grep -v '55:55' || true)
    ct_wan=$(grep "$WAN_PORT" /proc/net/nf_conntrack 2>/dev/null || true)
    
    printf '[%02d %s]\n' "$i" "$ts" >> "$LOGFILE"
    if [ -n "$bind_entries" ]; then
        echo "  PPE_BIND (L3):" >> "$LOGFILE"
        echo "$bind_entries" | sed 's/^/    /' >> "$LOGFILE"
    else
        echo "  PPE_BIND (L3): (none)" >> "$LOGFILE"
    fi
    if [ -n "$all_l2b" ]; then
        echo "  PPE_L2B (Bridge):" >> "$LOGFILE"
        echo "$all_l2b" | sed 's/^/    /' >> "$LOGFILE"
    else
        echo "  PPE_L2B (Bridge): (none)" >> "$LOGFILE"
    fi
    if [ -n "$ct_wan" ]; then
        echo "  CT_WAN: $ct_wan" >> "$LOGFILE"
    fi
    
    i=$(( i + 1 ))
    sleep 1
done

wait "$CLIENT_PID" || true
echo "[+] Concurrent dual-stream run finished."
cat "$LOGFILE"
echo "--- Client Execution Result ---"
cat /tmp/dual_client.log || true

# Cleanup
fttrctl exec --wait --timeout 5 "$ONU_MAC" "rm -f /tmp/run_dual.sh" > /dev/null 2>&1 || true
rm -f /tmp/dual_client.log
