/* Experimental HM2004-DU MAC-bit probe, NOT a verified FEC control.
 * Build: zig cc -target aarch64-linux-musl -static -O2 -Wall -Wextra
 *        fec-register-trial.c -o fec-register-trial
 * Run --self-test on Linux before hardware use. No arguments runs one trial.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define ADDRESS 0x01000014u
#define MASK 1u
#define READ_REG 0xc008a501UL
#define WRITE_REG 0x4008a500UL
struct regop { uint32_t addr, val; };
struct backend { int fd; volatile uint32_t *mock; };
static volatile sig_atomic_t interrupted;

static void on_signal(int sig) { (void)sig; interrupted = 1; }
static int get(struct backend b, uint32_t *value)
{
    if (b.mock) { *value = *b.mock; return 0; }
    struct regop op = { ADDRESS, 0 };
    if (ioctl(b.fd, READ_REG, &op) < 0) return -1;
    *value = op.val;
    return 0;
}
static int put(struct backend b, uint32_t value)
{
    if (b.mock) { *b.mock = value; return 0; }
    struct regop op = { ADDRESS, value };
    return ioctl(b.fd, WRITE_REG, &op);
}
static void pause_ms(unsigned ms)
{
    struct timespec remaining = { ms / 1000, (ms % 1000) * 1000000L };
    while (nanosleep(&remaining, &remaining) && errno == EINTR && !interrupted) {}
}
static int restore(struct backend b, uint32_t original)
{
    uint32_t current, after;
    if (get(b, &current)) return -1;
    uint32_t desired = (current & ~MASK) | (original & MASK);
    if (desired != current && put(b, desired)) return -1;
    if (get(b, &after)) return -1;
    printf("restore address=%08x before=%08x after=%08x expected_bit=%u\n",
           ADDRESS, current, after, original & MASK);
    return (after & MASK) == (original & MASK) ? 0 : -1;
}
static int trial(struct backend b, unsigned hold_ms, unsigned watchdog_ms)
{
    uint32_t original, current, after;
    if (get(b, &original)) { perror("baseline read"); return 1; }
    printf("baseline address=%08x value=%08x mask=%08x\n", ADDRESS, original, MASK);
    /* This experiment was selected against the observed all-zero baseline. */
    if (original != 0) { fprintf(stderr, "Unexpected baseline; no write\n"); return 1; }
    int armed[2];
    if (pipe(armed)) return 1;
    pid_t watchdog = fork();
    if (watchdog < 0) { close(armed[0]); close(armed[1]); return 1; }
    if (!watchdog) {
        close(armed[0]);
        if (setsid() < 0) _exit(1);
        signal(SIGHUP, SIG_IGN);
        signal(SIGINT, SIG_IGN);
        signal(SIGTERM, SIG_IGN);
        signal(SIGPIPE, SIG_IGN);
        interrupted = 0;
        if (write(armed[1], "A", 1) != 1) _exit(1);
        close(armed[1]);
        pause_ms(watchdog_ms);
        int result = restore(b, original);
        fflush(stdout);
        _exit(result ? 1 : 0);
    }
    close(armed[1]);
    char ready;
    ssize_t received = read(armed[0], &ready, 1);
    close(armed[0]);
    int result = 1;
    if (received != 1 || ready != 'A') goto done;
    printf("watchdog_armed pid=%ld restore_after_ms=%u\n", (long)watchdog, watchdog_ms);
    if (get(b, &current) || current != original) {
        fprintf(stderr, "Baseline changed; no trial write\n"); goto done;
    }
    if (interrupted) goto done;
    /* The independent watchdog is armed BEFORE the first trial write. */
    if (put(b, original | MASK) || get(b, &after)) {
        perror("trial write/read"); goto done;
    }
    printf("trial address=%08x requested=%08x readback=%08x\n", ADDRESS, original | MASK, after);
    result = after == (original | MASK) ? 0 : 1;
    if (!result) pause_ms(hold_ms);
done:
    if (restore(b, original)) result = 1;
    int status;
    while (waitpid(watchdog, &status, 0) < 0) {
        if (errno != EINTR) return 1;
    }
    return result || !WIFEXITED(status) || WEXITSTATUS(status);
}
static int self_test(void)
{
    volatile uint32_t *state = mmap(NULL, sizeof(*state), PROT_READ | PROT_WRITE,
                                   MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (state == MAP_FAILED) return 1;
    struct backend b = { -1, state };
    *state = 0x11;
    if (restore(b, 0) || *state != 0x10) return 1; /* Preserve unrelated bits. */
    *state = 2;
    if (!trial(b, 20, 60) || *state != 2) return 1; /* Refuse changed baseline. */
    *state = 0;
    if (trial(b, 20, 60) || *state) return 1;
    pid_t worker = fork();
    if (worker < 0) return 1;
    if (!worker) _exit(trial(b, 1000, 200));
    unsigned elapsed;
    for (elapsed = 0; elapsed < 1000 && *state != 1; elapsed++) pause_ms(1);
    if (*state != 1) { kill(worker, SIGKILL); waitpid(worker, NULL, 0); return 1; }
    kill(worker, SIGKILL);
    waitpid(worker, NULL, 0);
    for (elapsed = 0; elapsed < 1000 && *state != 0; elapsed++) pause_ms(1);
    int result = *state != 0;
    munmap((void *)state, sizeof(*state));
    printf("self_test=%s (normal restore, unrelated bits, baseline guard, worker SIGKILL)\n",
           result ? "FAIL" : "PASS");
    return result;
}
int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    signal(SIGPIPE, SIG_IGN);
    if (argc == 2 && !strcmp(argv[1], "--self-test")) return self_test();
    if (argc != 1) { fprintf(stderr, "Usage: %s [--self-test]\n", argv[0]); return 1; }
    signal(SIGHUP, on_signal);
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    int lock = open("/tmp/fttr-fec-register-trial.lock", O_CREAT | O_RDWR | O_NOFOLLOW, 0600);
    if (lock < 0 || flock(lock, LOCK_EX | LOCK_NB)) { perror("trial lock"); return 1; }
    int fd = open("/dev/fmcs_mci", O_RDWR);
    if (fd < 0) { perror("fmcs_mci"); return 1; }
    puts("EXPERIMENTAL MAC BIT; FEC SEMANTICS UNVERIFIED");
    int result = trial((struct backend){ fd, NULL }, 3000, 5000);
    close(fd);
    close(lock);
    return result;
}
