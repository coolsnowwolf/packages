#!/bin/sh
set -eu

test "$1" = 51ddns-agent
test "$2" = 0.6.7-1
test "$(51ddns-agent --version)" = "51ddns-agent 0.6.7-r7"
