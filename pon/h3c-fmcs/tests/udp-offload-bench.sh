#!/bin/sh
set -e

if [ "$#" -lt 2 ]; then
    echo "usage: $0 ONU_MAC WAN_IP [PORT=50001] [DURATION=15]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP="$2"
PORT="${3:-50001}"
DURATION="${4:-15}"
LOGFILE="/tmp/udp_bench_samples.log"

rm -f "$LOGFILE"

echo "[*] Step 1: Setting up client script on $ONU_MAC..."
fttrctl exec --wait --timeout 10 "$ONU_MAC" "echo '#!/bin/sh' > /tmp/run_udp.sh; echo 'end=\$(( \$(date +%s) + \$3 ))' >> /tmp/run_udp.sh; echo 'while [ \$(date +%s) -lt \$end ]; do dd if=/dev/zero bs=1024 count=40 2>/dev/null; sleep 0.02; done | /usr/bin/nc -u -p 50002 \$1 \$2 > /dev/null' >> /tmp/run_udp.sh; chmod +x /tmp/run_udp.sh"

echo "[*] Step 2: Dispatching UDP traffic to $WAN_IP:$PORT for ${DURATION}s..."
fttrctl exec --wait --timeout $(( DURATION + 10 )) "$ONU_MAC" "/tmp/run_udp.sh $WAN_IP $PORT $DURATION" > /dev/null 2>&1 &
CLIENT_PID=$!

echo "[*] Step 3: Sampling PPE bind entries and conntrack progression..."
i=1
while [ "$i" -le $(( DURATION + 5 )) ]; do
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
echo "[+] UDP benchmark sampling finished."
cat "$LOGFILE"
