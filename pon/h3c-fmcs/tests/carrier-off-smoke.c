/* Target-device integration smoke test. Requires an unused fttr16, carrier=0.
 * Verifies ioctl bounds and repeated synchronous cleanup without disrupting an
 * active ONU. This does not prove deletion of a populated hardware flow.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>

struct carrier_req {
	uint32_t port;
	uint32_t carrier;
};

#define SET_CARRIER _IOW(0xa5, 7, struct carrier_req)

int main(void)
{
	struct carrier_req req = { .port = 16, .carrier = 0 };
	FILE *state = fopen("/sys/class/net/fttr16/carrier", "r");
	int carrier, fd, i;

	if (!state) {
		perror("read fttr16 carrier");
		return 1;
	}
	i = fscanf(state, "%d", &carrier);
	fclose(state);
	if (i != 1 || carrier != 0) {
		fputs("fttr16 must be unused with carrier=0\n", stderr);
		return 1;
	}
	fd = open("/dev/fmcs_mci", O_RDWR);
	if (fd < 0) {
		perror("open fmcs");
		return 1;
	}
	for (i = 0; i < 3; i++) {
		if (ioctl(fd, SET_CARRIER, &req)) {
			perror("carrier-off cleanup");
			close(fd);
			return 1;
		}
	}
	for (i = 0; i < 2; i++) {
		req.port = i ? 17 : 0;
		errno = 0;
		if (ioctl(fd, SET_CARRIER, &req) != -1 || errno != EINVAL) {
			fputs("out-of-range port must return EINVAL\n", stderr);
			close(fd);
			return 1;
		}
	}
	close(fd);
	puts("PASS: repeated fttr16 carrier-off; ports 0/17 rejected");
	return 0;
}
