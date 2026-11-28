#!/bin/sh
# Read-only device inspection. Run with: sh offload-inspect.sh [sample-seconds]
set -eu
interval=${1:-5}
case "$interval" in ''|*[!0-9]*) echo 'sample-seconds must be 1..60' >&2; exit 2;; esac
[ "$interval" -ge 1 ] && [ "$interval" -le 60 ] || exit 2
net=/sys/class/net
ppe=/sys/kernel/debug/ppe
echo "kernel=$(uname -r) sample_seconds=$interval"
for mode in routed bridge; do
mask=/sys/module/h3c_fmcs/parameters/${mode}_offload_mask
if [ -r "$mask" ]; then
    echo "${mode}_offload_mask=$(cat "$mask")"
else
    echo "${mode}_offload_mask=unknown"
fi
done
if [ -r "$ppe/config" ]; then cat "$ppe/config"; else echo 'ppe_config=unavailable'; fi
if [ -r "$ppe/bind" ]; then
    echo "system_bound_entry_lines=$(wc -l < "$ppe/bind") (global, not per-ONU hit proof)"
else
    echo 'system_bound_entries=unknown'
fi
echo '--- flowtable ingress devices ---'
if command -v nft >/dev/null 2>&1; then nft list flowtables || echo 'flowtables=unavailable'; else echo 'nft=unavailable'; fi
echo '--- topology and initial counters ---'
snapshot=$(
    for port in $(seq 1 16); do
        dev=fttr$port
        base=$net/$dev
        [ -d "$base" ] || continue
        printf '%s %s %s %s %s\n' "$dev" \
            "$(cat "$base/statistics/rx_bytes")" "$(cat "$base/statistics/tx_bytes")" \
            "$(cat "$base/statistics/rx_packets")" "$(cat "$base/statistics/tx_packets")"
    done
)
for port in $(seq 0 16); do
    dev=fttr$port
    base=$net/$dev
    [ -d "$base" ] || continue
    printf '%s ifindex=%s iflink=%s carrier=%s mtu=%s lower=%s\n' "$dev" \
        "$(cat "$base/ifindex")" "$(cat "$base/iflink")" "$(cat "$base/carrier")" \
        "$(cat "$base/mtu")" "$(readlink "$base/lower_fttr0" || true)"
    echo "master=$(readlink "$base/master" || true) state=$(cat "$base/brport/state" 2>/dev/null || true) learning=$(cat "$base/brport/learning" 2>/dev/null || true)"
done
printf '%s\n' "$snapshot"
sleep "$interval"
echo '--- software counter deltas (not hardware throughput) ---'
printf '%s\n' "$snapshot" | while read -r dev rx tx rxp txp; do
    base=$net/$dev/statistics
    [ -d "$base" ] || continue
    now_rx=$(cat "$base/rx_bytes"); now_tx=$(cat "$base/tx_bytes")
    now_rxp=$(cat "$base/rx_packets"); now_txp=$(cat "$base/tx_packets")
    if [ "$now_rx" -lt "$rx" ] || [ "$now_tx" -lt "$tx" ] || \
       [ "$now_rxp" -lt "$rxp" ] || [ "$now_txp" -lt "$txp" ]; then
        echo "$dev counter_reset; delta unavailable"
    else
        echo "$dev rx_bytes=$((now_rx-rx)) tx_bytes=$((now_tx-tx)) rx_packets=$((now_rxp-rxp)) tx_packets=$((now_txp-txp))"
    fi
done
echo 'PPE verification requires a transiting TCP/UDP flow and matching bound/hit evidence.'
echo 'OMCI, CWMP, router-local traffic and software counters alone do not verify PPE.'
