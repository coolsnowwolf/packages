# Experimental MAC register trial

`fec-register-trial.c` is a standalone Linux diagnostic, not part of the installed
FEC API. The meaning of the candidate bit is **unverified**. A successful write
and readback do not establish FEC support or enablement.

## Candidate and scope

- Fixed raw FPGA address `0x01000014`, bit 0; no arbitrary address arguments.
- Stock `ploam` code at ELF VA `0x40ac68` writes table index `0x22` to 1.
  The matching `libminiolt` table maps this index to `0x01000014`.
- That block also initializes pattern words and may be test setup. Its runtime
  invocation and FEC semantics have not been established.
- The trial requires an exact zero baseline, changes the bit for 3 seconds,
  then restores it. It affects shared MAC state, not one selected ONU.

## Build and verify

```sh
cc -std=c11 -O2 -Wall -Wextra -Werror fec-register-trial.c -o fec-register-trial-host
./fec-register-trial-host --self-test
zig cc -target aarch64-linux-musl -static -std=c11 -O2 -Wall -Wextra -Werror \
    fec-register-trial.c -o fec-register-trial
```

The self-test uses anonymous shared memory, never the hardware device. It checks
normal restoration, preservation of unrelated bits, refusal of a changed
baseline, and restoration after the main trial process receives SIGKILL.

Before the hardware run, verify the board/bitstream, save current registration,
generation and traffic baselines, and run the self-test on the target as well.
Run the binary without arguments for one trial; sample status concurrently.
No daemon restart or persistent configuration change is needed.

The tool opens `/dev/fmcs_mci` and takes its own experiment flock. A separate
session's watchdog is armed before the first write and attempts restoration at
5 seconds, including when the main process exits early. The normal path also
restores and waits for the watchdog result. Both restore only the candidate bit,
preserving other bits as read at restoration. The experiment lock does not
serialize unrelated raw-register writers: do not run another register writer
against this address concurrently. Recovery depends on a functioning kernel and
SPI transport; inspect the explicit restore readback and exit status.

## First hardware result

On HM2004-DU with the stock-matching `FTTR_TOP.sbit` SHA256
`64d583215331d8b854dcc313be1fb82e171d601f5c11e45fd84d483c17bbc4ff`:

- Baseline `0`; trial write/readback `1`; normal and watchdog restoration `0`.
- Four registered ONUs remained O5, business verified, at unchanged generations
  throughout ten approximately one-second status samples.
- Healthy controls continued receiving and transmitting. The target
  TY-FS803G's upstream byte counter did not increase.
- Available completed-interval FEC PM remained zero. Cached 15-minute PM cannot
  diagnose a 3-second pulse; no instantaneous ONU FEC state was measured.

This validates the trial/restore path and candidate writability only. It neither
identifies the bit as FEC nor rules out FEC implemented elsewhere. The bit was
left at its original value. A longer pulse or other candidate requires a new
discriminating observation, such as instantaneous ONU FEC state or decoded
hardware status, rather than just write success or O5.
