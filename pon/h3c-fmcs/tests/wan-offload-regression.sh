#!/bin/sh
set -e

if [ "$#" -lt 2 ]; then
    echo "usage: $0 ONU_MAC WAN_IP [PORT=49361]" >&2
    exit 2
fi

ONU_MAC="$1"
WAN_IP="$2"
PORT="${3:-49361}"
SAMPLE_LOG='/tmp/wan_regression_samples.log'
CLIENT_OUT='/tmp/wan_client.out'

rm -f "$SAMPLE_LOG" "$CLIENT_OUT"

echo "[*] Step 1: Configuring WAN offload in UCI..."
uci set fttr.offload.onu_bridge='0'
uci set fttr.offload.wan_upstream='1'
uci set fttr.offload.wan_downstream='1'
uci commit fttr

# Wait for fttrd to reconcile
echo "[*] Waiting for fttrd reconciliation..."
for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    dir=$(cat /sys/module/airoha_eth/parameters/fttr_offload_directions 2>/dev/null || echo 0)
    rmask=$(cat /sys/module/h3c_fmcs/parameters/routed_offload_mask 2>/dev/null || echo 0)
    if [ "$dir" = "3" ] && [ "$rmask" != "0" ]; then
        echo "[+] Reconciled: directions=$dir routed_mask=$rmask"
        break
    fi
    sleep 1
done

echo "[*] Step 2: Dispatching ONU download on $ONU_MAC..."
fttrctl exec --wait --timeout 70 --json "$ONU_MAC" "sleep 60 | nc $WAN_IP $PORT | sha256sum" > "$CLIENT_OUT" &
EXEC_PID=$!

echo "[*] Step 3: Sampling traffic & offload progression..."
s=1
while [ "$s" -le 52 ]; do
    ts=$(date +%T)
    dir=$(cat /sys/module/airoha_eth/parameters/fttr_offload_directions 2>/dev/null || echo -)
    rmask=$(cat /sys/module/h3c_fmcs/parameters/routed_offload_mask 2>/dev/null || echo -)
    bmask=$(cat /sys/module/h3c_fmcs/parameters/bridge_offload_mask 2>/dev/null || echo -)
    bind_lines=$(cat /sys/kernel/debug/ppe/bind 2>/dev/null | grep "$PORT" || true)
    ct_line=$(grep "dport=$PORT\|sport=$PORT" /proc/net/nf_conntrack 2>/dev/null || true)
    
    printf 'SAMPLE %02d [%s] dir=%s rmask=%s bmask=%s\n' "$s" "$ts" "$dir" "$rmask" "$bmask" >> "$SAMPLE_LOG"
    if [ -n "$bind_lines" ]; then
        printf '  PPE_BIND: %s\n' "$bind_lines" >> "$SAMPLE_LOG"
    else
        printf '  PPE_BIND: (none)\n' >> "$SAMPLE_LOG"
    fi
    if [ -n "$ct_line" ]; then
        printf '  CONNTRACK: %s\n' "$ct_line" >> "$SAMPLE_LOG"
    fi

    # Trigger firewall reload at sample 8
    if [ "$s" -eq 8 ]; then
        echo "[*] [Sample 8] Triggering fw4 reload..."
        echo "TRIGGER: fw4 reload" >> "$SAMPLE_LOG"
        fw4 reload >/dev/null 2>&1 || true
    fi

    # Dynamically disable offload at sample 16
    if [ "$s" -eq 16 ]; then
        echo "[*] [Sample 16] Triggering dynamic withdrawal (offload disabled)..."
        echo "TRIGGER: withdraw offload" >> "$SAMPLE_LOG"
        uci set fttr.offload.wan_upstream='0'
        uci set fttr.offload.wan_downstream='0'
        uci set fttr.offload.onu_bridge='1'
        uci commit fttr
    fi

    s=$((s + 1))
    sleep 1
done

echo "[*] Waiting for client exec to complete..."
wait "$EXEC_PID" || true
echo "[+] Client execution completed."
cat "$CLIENT_OUT"
