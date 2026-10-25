#!/bin/sh
set -eu
# Usage: CXX=<target-g++> STAGING_DIR=<staging-dir> ./build-ctr-test.sh <prepared-ZeroTier-source> <output>
source_dir=$1
output=$2
"${CXX:-c++}" ${CXXFLAGS:-} -O2 -std=c++17 -pthread -ffunction-sections -fdata-sections \
 -I"$source_dir/node" "$(dirname "$0")/eip93-ctr.cpp" \
 "$source_dir/node/AES.cpp" "$source_dir/node/AES_armcrypto.cpp" \
 "$source_dir/node/AES_aesni.cpp" "$source_dir/node/Utils.cpp" \
 -Wl,--gc-sections,--wrap=sendmsg,--wrap=read,--wrap=socket -o "$output"
