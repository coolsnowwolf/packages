# AN7581 EIP93 with OpenVPN DCO

On QWRT's AN7581 target, `kmod-ovpn-backports` pulls in the Airoha EIP93
module and the kernel GCM template. OpenVPN's existing DCO path then uses
Linux Crypto API selection; no OpenSSL engine or OpenVPN crypto fork is
needed. Other targets retain their normal Crypto API selection. AN7583 is
not forced to use EIP93: its CPU AES/PMULL implementation needs a separate
end-to-end comparison.

## Configuration and verification

Build `openvpn-openssl` with `OPENVPN_openssl_ENABLE_DCO=y` (the default).
Use a DCO-compatible TUN configuration and `data-ciphers AES-256-GCM` on
both endpoints. Remove `disable-dco`, compression and `fragment` options.
A compatible ordinary userspace OpenVPN peer can interoperate with DCO;
the peer does not need EIP93 or DCO. These tests used OpenVPN 2.7.4 on the
router and 2.6.14 on the peer, over UDP.

The board DT must expose a working EIP93 device. A package dependency
cannot repair missing DT or unsupported kernel drivers. Confirm the
OpenVPN log reports the DCO device, then inspect `/proc/crypto` while the
tunnel is active. The expected GCM driver on AN7581 is:

```
gcm_base(ctr(aes-eip93),ghash-generic)
```

This is hybrid GCM: hardware AES-CTR plus software GHASH. Compare the
`1fb70000.crypto` line in `/proc/interrupts` before and after sustained VPN
traffic. Registration alone does not prove use. Restart the tunnel after
loading the hardware driver: existing transforms retain their provider.
For the ordinary userspace control, add `disable-dco` and restart OpenVPN.
For a DCO software control, stop test tunnels, unload `gcm` and the idle
EIP93 module, reload `gcm`, then restart the tunnel. Do not unload a driver
that is serving other workloads.

## Hardware comparison, 2026-10-22

Nokia XG-040G-MD (UBI), AN7581, BSP Linux 5.4.302, CPU fixed at 1.2 GHz
for comparison. AES-256-GCM, one TCP stream through the UDP VPN. Each
sample transfers 32 MiB in each direction; three samples per case.
RX/TX are from the router's perspective. Payload SHA256 was verified in
both directions for every sample. These are application payload rates,
not raw AES or line-rate figures.

| Router role | Path | RX Mbps | TX Mbps |
| --- | --- | ---: | ---: |
| Client | Userspace | 69.9 | 70.4 |
| Client | DCO, software GCM | 112.1 | 109.3 |
| Client | DCO, EIP93 hybrid GCM | 159.5 | 135.5 |
| Server | Userspace | 87.4 | 93.7 |
| Server | DCO, software GCM | 112.3 | 110.6 |
| Server | DCO, EIP93 hybrid GCM | 160.2 | 133.5 |

The hardware cases added 4771 and 4571 EIP93 interrupts respectively.
Relative to software DCO, hardware improved RX about 42% and TX about
21–24%. Userspace server samples varied (RX 68.0–98.2 Mbps); these small
samples are not a universal performance guarantee.

Direct AES-256-GCM microbenchmarks with 1400-byte payloads and 8-byte AAD
averaged 188.8 Mbps for OpenSSL software, 140.4 Mbps for AF_ALG hybrid GCM,
and 152.1 Mbps for direct cryptodev hybrid GCM. Ciphertext/tag equality,
decryption and corrupted-tag rejection passed. These algorithm numbers
are not comparable directly to VPN throughput. They show why introducing
another userspace crypto backend is not justified here. OpenSSL's current
devcrypto engine does not expose GCM; direct cryptodev tests used the
CIOCAUTHCRYPT interface, not that engine.

The existing EIP197 asynchronous-crypto workaround is unchanged. This
integration is validated on AN7581 without CONFIG_CRYPTO_DEV_SAFEXCEL;
it does not establish acceleration for kernels enabling that workaround.

## Control-channel prerequisite

Keep QWRT's OpenSSL devcrypto CTR fixes and EIP93 next-IV carry fix when
using a globally enabled devcrypto engine. DCO handles only the data
channel. An unrelated devcrypto partial-block offset bug was reproduced
with the original OpenSSL 3.6.3-1 package: resetting the IV reused stale
keystream and broke dynamic tls-crypt renegotiation in both ordinary and
DCO OpenVPN. Disabling DCO alone does not fix that control-channel bug.
The QWRT OpenSSL 3.6.3-2 package fixes the offset and exact-block handling;
its `tests/devcrypto-ctr-reset.c` also checks counter carry and copies.

Final package acceptance used standard client/server configurations with
`reneg-sec 5`, the default devcrypto engine still enabled, OpenSSL 3.6.3-2,
the corrected BSP EIP93 module and ovpn-backports release 6. All 12
bidirectional samples (four role/mode combinations) passed SHA256 checks.
Each case completed 6–10 TLS handshakes with no TLS authentication errors.
Under this frequent-renegotiation stress, DCO hybrid GCM averaged
155.6/130.7 Mbps as client and 155.6/131.4 Mbps as server; userspace controls
averaged 68.6/66.8 and 64.1/84.7 Mbps respectively. Control-channel CTR can
also increase EIP93 counters in userspace mode; those interrupts alone are
not evidence of data-channel offload.

After deployment, the 63-case CTR regression and all nine GCM correctness
checks (three sizes across software, AF_ALG and cryptodev, including
corrupted-tag rejection) passed. This acceptance covers the specified
AN7581 BSP 5.4 board, not every OpenVPN feature or kernel/SoC combination.
