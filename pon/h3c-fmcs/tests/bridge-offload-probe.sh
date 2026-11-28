#!/bin/sh
# Run on the MOLT after checking fttrctl status: ports/ONU IDs are not stable.
set -eu
if [ "$#" -lt 7 ] || [ "$#" -gt 8 ]; then
    echo "usage: $0 SRC_PORT DST_PORT SRC_MAC DST_MAC SRC_IP DST_IP software|hardware|revoke|auto|auto-recover [MiB=16]" >&2
    exit 2
fi
src_port=$1 dst_port=$2 src_mac=$3 dst_mac=$4 src_ip=$5 dst_ip=$6 mode=$7
chunks=${8:-16}
managed=0
case "$mode" in auto|auto-recover) managed=1;; esac
case "$chunks" in ''|*[!0-9]*|0*) exit 2;; esac
[ "${#chunks}" -le 3 ] && [ "$chunks" -ge 16 ] && [ "$chunks" -le 120 ]
[ "$mode" != auto-recover ] || [ "$chunks" -ge 24 ]
for number in "$src_port" "$dst_port"; do
    case "$number" in ''|*[!0-9]*|0*) exit 2;; esac
    [ "${#number}" -le 2 ] && [ "$number" -le 16 ]
done
[ "$src_port" != "$dst_port" ]
for mac in "$src_mac" "$dst_mac"; do
    case "$mac" in ''|*[!0-9A-Fa-f]*) exit 2;; esac
    [ "${#mac}" = 12 ]
done
is_ipv6=0
case "$src_ip" in *:* ) is_ipv6=1;; esac
normalize_ipv6() {
    _ip=$1
    case "$_ip" in
        *::* )
            _left=${_ip%%::*}
            _right=${_ip#*::}
            ;;
        * )
            _left=$_ip
            _right=""
            ;;
    esac
    _l_cnt=0
    _r_cnt=0
    _l_res=""
    _r_res=""
    if [ -n "$_left" ]; then
        _oIFS=$IFS; IFS=:
        set -- $_left
        IFS=$_oIFS
        _l_cnt=$#
        for _w in "$@"; do
            _h=$(printf '%04x' "0x$_w")
            _l_res="${_l_res}${_h}:"
        done
    fi
    if [ -n "$_right" ]; then
        _oIFS=$IFS; IFS=:
        set -- $_right
        IFS=$_oIFS
        _r_cnt=$#
        for _w in "$@"; do
            _h=$(printf '%04x' "0x$_w")
            _r_res="${_r_res}${_h}:"
        done
    fi
    _zeros=$((8 - _l_cnt - _r_cnt))
    _z_res=""
    _i=0
    while [ "$_i" -lt "$_zeros" ]; do
        _z_res="${_z_res}0000:"
        _i=$((_i + 1))
    done
    _full="${_l_res}${_z_res}${_r_res}"
    echo "${_full%:}"
}
if [ "$is_ipv6" = 0 ]; then
    for ip in "$src_ip" "$dst_ip"; do
        case "$ip" in ''|*[!0-9.]*) exit 2;; esac
    done
    src_match="$src_ip"
    dst_match="$dst_ip"
else
    case "$dst_ip" in *:* ) ;; *) exit 2;; esac
    for ip in "$src_ip" "$dst_ip"; do
        case "$ip" in ''|*[!0-9A-Fa-f:]*) exit 2;; esac
    done
    src_match=$(normalize_ipv6 "$src_ip")
    dst_match=$(normalize_ipv6 "$dst_ip")
fi
[ "$src_mac" != "$dst_mac" ] && [ "$src_ip" != "$dst_ip" ]
mask=/sys/module/h3c_fmcs/parameters/bridge_offload_mask
[ "$managed" = 1 ] || [ "$(cat "$mask")" = 0 ]
[ "$managed" = 1 ] || [ "$(uci -q get fttr.offload.enabled)" = 0 ]
[ "$(cat /sys/module/h3c_fmcs/parameters/routed_offload_mask)" = 0 ]
for port in "$src_port" "$dst_port"; do
    [ "$(cat /sys/class/net/fttr"$port"/carrier)" = 1 ]
    [ "$(cat /sys/class/net/fttr"$port"/brport/state)" = 3 ]
    [ "$(readlink /sys/class/net/fttr"$port"/master)" = '../br-lan' ]
done
case "$mode" in
    software) probe_mask=0;;
    hardware|revoke|auto|auto-recover) probe_mask=$(((1 << (src_port - 1)) | (1 << (dst_port - 1))));;
    *) exit 2;;
esac
! nft list table bridge fmcs_pair_probe >/dev/null 2>&1
tmp=$(mktemp -d /tmp/fmcs-pair-probe.XXXXXX)
table_created=0 restore_learning=0 sender= receiver=
exec_checked() {
    result=$(fttrctl exec --wait --timeout 10 --json "$1" "$2") || {
        printf '%s\n' "$result"
        return 1
    }
    printf '%s\n' "$result"
    [ "$(printf '%s' "$result" | jsonfilter -e '@.status')" = executed ] &&
        [ "$(printf '%s' "$result" | jsonfilter -e '@.exit_code')" = 0 ]
}
cleanup() {
    set +e
    [ "$restore_learning" = 0 ] || ip link set dev "fttr$dst_port" type bridge_slave learning on
    [ "$managed" = 1 ] || printf '0\n' > "$mask"
    [ "$table_created" = 0 ] || nft delete table bridge fmcs_pair_probe
    [ -z "$sender" ] || wait "$sender"
    [ -z "$receiver" ] || wait "$receiver"
    rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
# A forgotten routed test must not masquerade as a bridge test.
if [ "$is_ipv6" = 0 ]; then
    exec_checked "$src_mac" "ip route get $dst_ip from $src_ip && ! ip route get $dst_ip from $src_ip | grep -q via && command -v sha256sum"
    exec_checked "$dst_mac" "ip route get $src_ip from $dst_ip && ! ip route get $src_ip from $dst_ip | grep -q via && command -v sha256sum"
else
    exec_checked "$src_mac" "ip -6 route get $dst_ip from $src_ip && ! ip -6 route get $dst_ip from $src_ip | grep -q via && command -v sha256sum"
    exec_checked "$dst_mac" "ip -6 route get $src_ip from $dst_ip && ! ip -6 route get $src_ip from $dst_ip | grep -q via && command -v sha256sum"
fi
# Counts after the normal fw4 bridge policy; never adds an early accept rule.
if [ "$managed" = 0 ]; then
if [ "$is_ipv6" = 0 ]; then
nft -f - <<NFT
add table bridge fmcs_pair_probe
add flowtable bridge fmcs_pair_probe pair { hook ingress priority 1; devices = { fttr$src_port, fttr$dst_port }; flags offload; counter; }
add chain bridge fmcs_pair_probe forward { type filter hook forward priority 10; policy accept; }
add rule bridge fmcs_pair_probe forward ether type ip ip saddr $src_ip ip daddr $dst_ip tcp dport 49176 counter flow add @pair
add rule bridge fmcs_pair_probe forward ether type ip ip saddr $dst_ip ip daddr $src_ip tcp sport 49176 counter flow add @pair
NFT
else
nft -f - <<NFT
add table bridge fmcs_pair_probe
add flowtable bridge fmcs_pair_probe pair { hook ingress priority 1; devices = { fttr$src_port, fttr$dst_port }; flags offload; counter; }
add chain bridge fmcs_pair_probe forward { type filter hook forward priority 10; policy accept; }
add rule bridge fmcs_pair_probe forward ether type ip6 ip6 saddr $src_ip ip6 daddr $dst_ip tcp dport 49176 counter flow add @pair
add rule bridge fmcs_pair_probe forward ether type ip6 ip6 saddr $dst_ip ip6 daddr $src_ip tcp sport 49176 counter flow add @pair
NFT
fi
table_created=1
fi
echo 'logical counters before transfer (rx_bytes, tx_bytes):'
for port in "$src_port" "$dst_port"; do
    echo "fttr$port"
    cat /sys/class/net/fttr"$port"/statistics/rx_bytes /sys/class/net/fttr"$port"/statistics/tx_bytes
done
[ "$managed" = 1 ] || printf '%s\n' "$probe_mask" > "$mask"
# Keep nc's stdin open: an immediate FIN from a closed exec stdin puts the
# conntrack in CLOSE_WAIT and prevents any established-flow admission.
listen=$((chunks + 9))
fttrctl exec --wait --timeout "$((listen + 10))" --json "$dst_mac" "sleep $listen | nc -l -p 49176 -w $listen | sha256sum" > "$tmp/receiver" &
receiver=$!
sleep 2
send_command="(i=0; while [ \"\$i\" -lt $chunks ]; do dd if=/dev/zero bs=1024 count=1024 2>/dev/null; sleep 1; i=\$((i+1)); done) | nc -w 3 $dst_ip 49176"
fttrctl exec --wait --timeout "$((chunks + 14))" --json "$src_mac" "$send_command" > "$tmp/sender" &
sender=$!
samples=$(((chunks - 4) / 4))
sample=0
while [ "$sample" -lt "$samples" ]; do
    sample=$((sample + 1))
    sleep 4
    echo "sample=$sample mode=$mode mask=$(cat "$mask")"
    bound=$(grep -F '49176' /sys/kernel/debug/ppe/bind | grep -F "$src_match" | grep -F "$dst_match" || true)
    printf '%s\n' "$bound"
    conntrack=$(grep 'dport=49176\|sport=49176' /proc/net/nf_conntrack | grep -F "$src_match" | grep -F "$dst_match" || true)
    printf '%s\n' "$conntrack"
    if [ "$managed" = 1 ]; then
        nft list flowtable bridge fw4 fb
    else
        nft list table bridge fmcs_pair_probe
    fi
    proto_label="IPv4"
    [ "$is_ipv6" = 0 ] || proto_label="IPv6"
    if [ "$mode" = hardware ] || [ "$mode" = auto ] ||
       { [ "$mode" = auto-recover ] && [ "$sample" != 3 ]; } ||
       { [ "$mode" = revoke ] && [ "$sample" -lt 3 ]; }; then
        [ "$(($(cat "$mask") & probe_mask))" = "$probe_mask" ]
        printf '%s\n' "$bound" | grep -F "BND $proto_label 5T orig=$src_match:" | grep -F -- "->$dst_match:49176" >/dev/null
        printf '%s\n' "$bound" | grep -F "BND $proto_label 5T orig=$dst_match:49176->" | grep -F -- "->$src_match:" >/dev/null
        printf '%s\n' "$conntrack" | grep -F 'dport=49176' | grep -F '[HW_OFFLOAD]' >/dev/null
    fi
    if [ "$mode" = auto-recover ] && [ "$sample" = 2 ]; then
        [ "$(cat /sys/class/net/fttr"$dst_port"/brport/learning)" = 1 ]
        echo 'Revoking populated flows with a selected-port bridge notification.'
        restore_learning=1
        ip link set dev "fttr$dst_port" type bridge_slave learning off
        ip link set dev "fttr$dst_port" type bridge_slave learning on
        restore_learning=0
        [ "$(cat "$mask")" = 0 ]
    elif [ "$mode" = auto-recover ] && [ "$sample" = 3 ]; then
        # Recovery can already be admitted after two stable polls. The immediate
        # zero above proves withdrawal; this sample is diagnostic, not a timer.
        echo 'Recovery settling sample; subsequent samples require bidirectional BND again.'
    fi
    if [ "$mode" = revoke ]; then
        if [ "$sample" = 2 ]; then
            echo 'Revoking populated hardware flows while the payload is still being sent.'
            printf '0\n' > "$mask"
        elif [ "$sample" = 3 ]; then
            [ "$(cat "$mask")" = 0 ]
            ! printf '%s\n' "$bound" | grep -F "BND $proto_label 5T orig=$src_match:" | grep -F -- "->$dst_match:49176" >/dev/null
            ! printf '%s\n' "$bound" | grep -F "BND $proto_label 5T orig=$dst_match:49176->" | grep -F -- "->$src_match:" >/dev/null
            ! printf '%s\n' "$conntrack" | grep -F 'dport=49176' | grep -F '[HW_OFFLOAD]' >/dev/null
        fi
    fi
done
wait "$sender"
sender=
wait "$receiver"
receiver=
cat "$tmp/sender" "$tmp/receiver"
echo 'logical counters after transfer (rx_bytes, tx_bytes):'
for port in "$src_port" "$dst_port"; do
    echo "fttr$port"
    cat /sys/class/net/fttr"$port"/statistics/rx_bytes /sys/class/net/fttr"$port"/statistics/tx_bytes
done
for endpoint in sender receiver; do
    [ "$(jsonfilter -i "$tmp/$endpoint" -e '@.status')" = executed ]
    [ "$(jsonfilter -i "$tmp/$endpoint" -e '@.exit_code')" = 0 ]
done
# Independent local expected payload; checks both content and exact byte count.
expected=$(dd if=/dev/zero bs=1024 count="$((chunks * 1024))" 2>/dev/null | sha256sum | awk '{print $1}')
[ "$(jsonfilter -i "$tmp/receiver" -e '@.output' | awk '{print $1}')" = "$expected" ]
echo "PASS: cross-ONU TCP payload hash matches $chunks MiB; hardware evidence must be judged separately."
