# EIP-93 CTR regression test

Build against prepared package sources (after applying the patches):

```sh
CXX=/path/to/target-g++ STAGING_DIR=/path/to/staging \
  sh tests/build-ctr-test.sh /path/to/ZeroTierOne-1.16.2 /tmp/ctr-test
```

On a Linux device without CPU AES, with `ctr(aes-eip93)` and AF_ALG
skcipher support loaded (`kmod-crypto-user`):

```sh
/tmp/ctr-test hardware
ZEROTIER_EIP93=off /tmp/ctr-test
```

The hardware run requires completed kernel reads and exercised fault
injections. The disabled run requires no kernel encryption requests.
Tests compare ciphertext with a blockwise AES-CTR reference, cover split
updates, partial blocks, unaligned buffers, in-place operation, stream
reinitialization with a new key, GMAC-SIV, concurrent workers, short I/O,
socket/send failures and descriptor cleanup.

Build and run without the `hardware` argument on a CPU-AES host as well.
To exercise unavailable-driver fallback on a host without EIP-93:

```sh
CXXFLAGS=-DZT_AES_NO_ACCEL sh tests/build-ctr-test.sh /path/to/source /tmp/ctr-sw
/tmp/ctr-sw
```

These are correctness and lifecycle tests, not throughput measurements.
