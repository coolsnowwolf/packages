#!/bin/sh
# Temporary routed TCP probe using two MQTT-managed H3C clients. Run on the MOLT.
set -eu
if [ "$#" -ne 7 ]; then
    echo "usage: $0 SRC_MAC DST_MAC SRC_IP DST_IP GATEWAY MASK CLIENT_ROUTE_TABLE" >&2
    exit 2
fi
src_mac=$1 dst_mac=$2 src_ip=$3 dst_ip=$4 gateway=$5 probe_mask=$6 route_table=$7
for mac in "$src_mac" "$dst_mac"; do
    case "$mac" in ''|*[!0-9A-Fa-f]*) exit 2;; esac
    [ "${#mac}" = 12 ] || exit 2
done
for ip in "$src_ip" "$dst_ip" "$gateway"; do
    case "$ip" in ''|*[!0-9.]*) exit 2;; esac
done
for number in "$probe_mask" "$route_table"; do
    case "$number" in ''|*[!0-9]*|0*) exit 2;; esac
    [ "${#number}" -le 5 ] || exit 2
done
[ "$probe_mask" -le 65535 ] && [ "$route_table" -le 65535 ]
[ "$src_mac" != "$dst_mac" ] && [ "$src_ip" != "$dst_ip" ]
[ -d /tmp ]
mask=/sys/module/h3c_fmcs/parameters/routed_offload_mask
redirects=/proc/sys/net/ipv4/conf/br-lan/send_redirects
old_mask=$(cat "$mask")
old_redirects=$(cat "$redirects")
# Changing the mask flushes flow state; require the default experimental state.
[ "$old_mask" = 0 ]
[ "$(cat /sys/module/h3c_fmcs/parameters/bridge_offload_mask)" = 0 ]
# The daemon must not re-arm bridge admission during a manual routed probe.
[ "$(uci -q get fttr.offload.enabled)" = 0 ]
[ "$(cat /proc/sys/net/ipv4/ip_forward)" = 1 ]
! nft list table inet fmcs_routed_probe >/dev/null 2>&1
tmp=$(mktemp -d /tmp/fmcs-routed-probe.XXXXXX)
table_created=0 src_route=0 dst_route=0 receiver= sender=
exec_checked() {
    result=$(fttrctl exec --wait --timeout 10 --json "$1" "$2")
    printf '%s\n' "$result"
    [ "$(printf '%s' "$result" | jsonfilter -e '@.status')" = executed ] &&
        [ "$(printf '%s' "$result" | jsonfilter -e '@.exit_code')" = 0 ]
}
cleanup() {
    set +e
    printf '%s\n' "$old_mask" > "$mask"
    [ "$table_created" = 0 ] || nft delete table inet fmcs_routed_probe
    printf '%s\n' "$old_redirects" > "$redirects"
    if [ "$src_route" = 1 ]; then
        exec_checked "$src_mac" "ip route del table $route_table $dst_ip/32 via $gateway dev br0 metric 42760" ||
            echo "CLEANUP UNCONFIRMED: $src_mac: ip route del table $route_table $dst_ip/32 via $gateway dev br0 metric 42760" >&2
    fi
    if [ "$dst_route" = 1 ]; then
        exec_checked "$dst_mac" "ip route del table $route_table $src_ip/32 via $gateway dev br0 metric 42760" ||
            echo "CLEANUP UNCONFIRMED: $dst_mac: ip route del table $route_table $src_ip/32 via $gateway dev br0 metric 42760" >&2
    fi
    # Child listeners and senders have their own finite timeouts.
    [ -z "$sender" ] || wait "$sender"
    [ -z "$receiver" ] || wait "$receiver"
    rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
# Refuse existing exact routes; never replace a client's configuration.
exec_checked "$src_mac" "test -z \"\$(ip route show table $route_table exact $dst_ip/32)\""
exec_checked "$dst_mac" "test -z \"\$(ip route show table $route_table exact $src_ip/32)\""
src_route=1
exec_checked "$src_mac" "ip route add table $route_table $dst_ip/32 via $gateway dev br0 metric 42760 && ip route get $dst_ip from $src_ip"
dst_route=1
exec_checked "$dst_mac" "ip route add table $route_table $src_ip/32 via $gateway dev br0 metric 42760 && ip route get $src_ip from $dst_ip"
printf '0\n' > "$redirects"
printf '%s\n' "$probe_mask" > "$mask"
nft -f - <<NFT
add table inet fmcs_routed_probe
add flowtable inet fmcs_routed_probe path { hook ingress priority -10; devices = { fttr0 }; flags offload; counter; }
add chain inet fmcs_routed_probe forward { type filter hook forward priority 10; policy accept; }
add rule inet fmcs_routed_probe forward ip saddr $src_ip ip daddr $dst_ip tcp dport 49175 counter flow add @path
add rule inet fmcs_routed_probe forward ip saddr $dst_ip ip daddr $src_ip tcp sport 49175 counter flow add @path
NFT
table_created=1
fttrctl exec --wait --timeout 35 --json "$dst_mac" 'sleep 25 | nc -l -p 49175 -w 25 | wc -c' > "$tmp/receiver" &
receiver=$!
sleep 2
send_command="(i=0; while [ \"\$i\" -lt 16 ]; do dd if=/dev/zero bs=1024 count=1024 2>/dev/null; sleep 1; i=\$((i+1)); done) | nc -w 3 $dst_ip 49175"
fttrctl exec --wait --timeout 30 --json "$src_mac" "$send_command" > "$tmp/sender" &
sender=$!
sleep 3
echo '--- active-flow evidence (UNB and OFFLOAD are not hardware-hit proof) ---'
cat /sys/kernel/debug/ppe/config
wc -l /sys/kernel/debug/ppe/bind
cat /sys/kernel/debug/ppe/bind
grep -F "$src_ip:" /sys/kernel/debug/ppe/entries || true
grep -F "$dst_ip:" /sys/kernel/debug/ppe/entries || true
grep 'dport=49175\|sport=49175' /proc/net/nf_conntrack || true
nft list table inet fmcs_routed_probe
wait "$sender"
sender=
wait "$receiver"
receiver=
cat "$tmp/sender" "$tmp/receiver"
[ "$(jsonfilter -i "$tmp/sender" -e '@.status')" = executed ]
[ "$(jsonfilter -i "$tmp/sender" -e '@.exit_code')" = 0 ]
[ "$(jsonfilter -i "$tmp/receiver" -e '@.status')" = executed ]
[ "$(jsonfilter -i "$tmp/receiver" -e '@.exit_code')" = 0 ]
[ "$(jsonfilter -i "$tmp/receiver" -e '@.output' | tr -d '[:space:]')" = 16777216 ]
echo 'PASS: receiver counted 16 MiB; inspect the evidence separately for offload.'
echo 'NPU attachment is lazy global initialization and may remain after cleanup.'
