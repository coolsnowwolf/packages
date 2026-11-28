#!/bin/sh
set -e

if [ "$#" -lt 2 ]; then
    echo "usage: $0 ONU_MAC WAN_IP6 [PORT=50003] [DURATION=10]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP6="$2"
PORT="${3:-50003}"
DURATION="${4:-10}"
LOGFILE="/tmp/ipv6_udp_bench_samples.log"

rm -f "$LOGFILE"

echo "[*] Step 1: Setting up IPv6 UDP client on $ONU_MAC to $WAN_IP6:$PORT..."
fttrctl exec --wait --timeout 10 "$ONU_MAC" "echo '#!/bin/sh' > /tmp/run_ipv6_udp.sh; echo 'end=\$(( \$(date +%s) + \$3 ))' >> /tmp/run_ipv6_udp.sh; echo 'while [ \$(date +%s) -lt \$end ]; do dd if=/dev/zero bs=1024 count=30 2>/dev/null; sleep 0.02; done | /bin/busybox nc -n -u -p 50005 \$1 \$2 > /dev/null' >> /tmp/run_ipv6_udp.sh; chmod +x /tmp/run_ipv6_udp.sh"

echo "[*] Step 2: Dispatching IPv6 UDP traffic for ${DURATION}s..."
fttrctl exec --wait --timeout $(( DURATION + 10 )) "$ONU_MAC" "/tmp/run_ipv6_udp.sh $WAN_IP6 $PORT $DURATION" > /dev/null 2>&1 &
CLIENT_PID=$!

echo "[*] Step 3: Sampling PPE bind entries and conntrack progression for IPv6 UDP..."
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
echo "[+] IPv6 UDP benchmark sampling finished."
cat "$LOGFILE"
