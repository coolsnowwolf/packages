#!/bin/sh
s1=$(cat /proc/stat | grep '^cpu ')
"$@"
s2=$(cat /proc/stat | grep '^cpu ')

u1=$(echo "$s1" | awk '{print $2}')
n1=$(echo "$s1" | awk '{print $3}')
k1=$(echo "$s1" | awk '{print $4}')
i1=$(echo "$s1" | awk '{print $5}')
w1=$(echo "$s1" | awk '{print $6}')
q1=$(echo "$s1" | awk '{print $7}')
sq1=$(echo "$s1" | awk '{print $8}')

u2=$(echo "$s2" | awk '{print $2}')
n2=$(echo "$s2" | awk '{print $3}')
k2=$(echo "$s2" | awk '{print $4}')
i2=$(echo "$s2" | awk '{print $5}')
w2=$(echo "$s2" | awk '{print $6}')
q2=$(echo "$s2" | awk '{print $7}')
sq2=$(echo "$s2" | awk '{print $8}')

total1=$(( u1 + n1 + k1 + i1 + w1 + q1 + sq1 ))
total2=$(( u2 + n2 + k2 + i2 + w2 + q2 + sq2 ))
idle1=$(( i1 + w1 ))
idle2=$(( i2 + w2 ))

diff_total=$(( total2 - total1 ))
diff_idle=$(( idle2 - idle1 ))
diff_used=$(( diff_total - diff_idle ))

if [ "$diff_total" -gt 0 ]; then
    cpu_pct=$(( (diff_used * 1000) / diff_total ))
    echo "CPU_USAGE: $(( cpu_pct / 10 )).$(( cpu_pct % 10 ))%"
else
    echo "CPU_USAGE: 0.0%"
fi
