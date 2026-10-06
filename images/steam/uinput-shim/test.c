/* What the shim must do for a program written like Steam's client: open
 * /dev/uinput, set up a gamepad with either uinput API, create it, report its
 * name, send events, and (mode "ff") answer the force feedback requests an app
 * makes. Run with LD_PRELOAD=libcha-uinput.so against the streamer's broker by
 * run-tests.sh (CHA_UINPUT_SOCK names its socket); built as i386 and amd64.
 *
 *   test <new|legacy|ff>
 *
 * Prints "sysname <name>" once the device exists, then waits for a line on
 * stdin before destroying it (ff: after serving force feedback until the app
 * erased an effect). Exits 0 if every check passed. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "kabi.h"

static void die(const char *what)
{
	fprintf(stderr, "FAIL: %s (errno %d: %s)\n", what, errno, strerror(errno));
	exit(1);
}

#define CHECK(x) do { if ((x) < 0) die(#x); } while (0)
#define EXPECT_FAIL(x, err) do { errno = 0; if ((x) >= 0 || errno != (err)) die("expected " #x " to fail with " #err); } while (0)

static const int abs_codes[] = {0, 1, 2, 3, 4, 5, 0x10, 0x11};

static void serve_ff(int fd)
{
	int uploaded = 0, played = 0, erased = 0, i;

	for (i = 0; i < 400 && !erased; i++) {
		struct pollfd pf = {.fd = fd, .events = POLLIN};
		struct k_input_event ev;

		if (poll(&pf, 1, 100) <= 0) continue;
		while (read(fd, &ev, sizeof ev) == (ssize_t)sizeof ev) {
			if (ev.type == K_EV_UINPUT && ev.code == K_UI_FF_UPLOAD) {
				struct k_uinput_ff_upload up;
				memset(&up, 0, sizeof up);
				up.request_id = ev.value;
				CHECK(ioctl(fd, UI_BEGIN_FF_UPLOAD, &up));
				if (up.effect.type != K_FF_RUMBLE || up.effect.u.raw[0] != 0x00 || up.effect.u.raw[1] != 0xc0 ||
				    up.effect.replay_length != 100)
					die("the uploaded effect isn't the app's rumble");
				up.retval = 0;
				CHECK(ioctl(fd, UI_END_FF_UPLOAD, &up));
				printf("ff upload ok id %d\n", up.effect.id);
				uploaded = 1;
			} else if (ev.type == K_EV_UINPUT && ev.code == K_UI_FF_ERASE) {
				struct k_uinput_ff_erase er;
				memset(&er, 0, sizeof er);
				er.request_id = ev.value;
				CHECK(ioctl(fd, UI_BEGIN_FF_ERASE, &er));
				/* The app's erase fails because this answer does. */
				er.retval = -EIO;
				CHECK(ioctl(fd, UI_END_FF_ERASE, &er));
				printf("ff erase %u\n", er.effect_id);
				erased = 1;
			} else if (ev.type == K_EV_FF) {
				printf("ff play %u %d\n", ev.code, ev.value);
				played = 1;
			}
			fflush(stdout);
		}
	}
	if (!uploaded || !played || !erased) die("force feedback: not all of upload, play and erase came");
	printf("ff done\n");
}

int main(int argc, char **argv)
{
	const char *mode = argc > 1 ? argv[1] : "new";
	int legacy = strcmp(mode, "legacy") == 0, ff = strcmp(mode, "ff") == 0;
	struct k_input_id id = {.bustype = 3, .vendor = 0x1234, .product = 0x5678, .version = 0x0100};
	char sysname[64], line[16];
	unsigned int version = 0;
	struct k_input_event ev[3];
	int fd, i, code, n;

	alarm(60);
	setvbuf(stdout, NULL, _IONBF, 0);
	fd = open("/dev/uinput", O_RDWR | O_NONBLOCK | O_CLOEXEC);
	if (fd < 0) die("open /dev/uinput");
	CHECK(ioctl(fd, UI_GET_VERSION, &version));
	if (version < 4) die("UI_GET_VERSION");

	CHECK(ioctl(fd, UI_SET_EVBIT, K_EV_KEY));
	CHECK(ioctl(fd, UI_SET_EVBIT, K_EV_ABS));
	CHECK(ioctl(fd, UI_SET_EVBIT, K_EV_FF));
	CHECK(ioctl(fd, UI_SET_EVBIT, K_EV_SYN));
	for (code = K_BTN_SOUTH; code <= 0x13e; code++) CHECK(ioctl(fd, UI_SET_KEYBIT, code));
	for (code = 0x220; code <= 0x223; code++) CHECK(ioctl(fd, UI_SET_KEYBIT, code));
	/* A keyboard key is refused. */
	EXPECT_FAIL(ioctl(fd, UI_SET_KEYBIT, 30), EINVAL);
	for (i = 0; i < (int)(sizeof abs_codes / sizeof *abs_codes); i++) CHECK(ioctl(fd, UI_SET_ABSBIT, abs_codes[i]));
	CHECK(ioctl(fd, UI_SET_FFBIT, K_FF_RUMBLE));
	CHECK(ioctl(fd, UI_SET_FFBIT, K_FF_PERIODIC));
	CHECK(ioctl(fd, UI_SET_FFBIT, K_FF_SINE));
	CHECK(ioctl(fd, UI_SET_FFBIT, K_FF_GAIN));
	CHECK(ioctl(fd, UI_SET_PHYS, "steam/virtual"));

	if (legacy) {
		struct k_uinput_user_dev ud;
		memset(&ud, 0, sizeof ud);
		strcpy(ud.name, "Shim Test Pad");
		ud.id = id;
		ud.ff_effects_max = 16;
		for (i = 0; i < (int)(sizeof abs_codes / sizeof *abs_codes); i++) {
			ud.absmin[abs_codes[i]] = -32768;
			ud.absmax[abs_codes[i]] = 32767;
		}
		if (write(fd, &ud, sizeof ud) != (ssize_t)sizeof ud) die("write uinput_user_dev");
	} else {
		struct k_uinput_setup us;
		memset(&us, 0, sizeof us);
		us.id = id;
		strcpy(us.name, "Shim Test Pad");
		us.ff_effects_max = 16;
		for (i = 0; i < (int)(sizeof abs_codes / sizeof *abs_codes); i++) {
			struct k_uinput_abs_setup as;
			memset(&as, 0, sizeof as);
			as.code = abs_codes[i];
			as.absinfo.minimum = -32768;
			as.absinfo.maximum = 32767;
			CHECK(ioctl(fd, UI_ABS_SETUP, &as));
		}
		CHECK(ioctl(fd, UI_DEV_SETUP, &us));
	}
	CHECK(ioctl(fd, UI_DEV_CREATE));
	n = ioctl(fd, UI_GET_SYSNAME(sizeof sysname), sysname);
	if (n <= 0) die("UI_GET_SYSNAME");
	if (n != (int)strlen(sysname) + 1) die("UI_GET_SYSNAME's length");

	memset(ev, 0, sizeof ev);
	ev[0].type = K_EV_KEY;
	ev[0].code = K_BTN_SOUTH;
	ev[0].value = 1;
	ev[1].type = K_EV_ABS;
	ev[1].code = 0;
	ev[1].value = 12000;
	ev[2].type = K_EV_SYN;
	if (write(fd, ev, sizeof ev) != (ssize_t)sizeof ev) die("write events");
	/* Refused: a keyboard key; and a short write isn't a whole event. */
	ev[0].code = 30;
	EXPECT_FAIL(write(fd, ev, sizeof ev[0]), EINVAL);
	EXPECT_FAIL(write(fd, ev, sizeof ev[0] - 1), EINVAL);
	/* Nothing to read yet, and a non-blocking read says so. */
	EXPECT_FAIL(read(fd, ev, sizeof ev[0]), EAGAIN);
	/* From here the app may use the device (the test's other half does). */
	printf("sysname %s\n", sysname);

	if (ff) serve_ff(fd);
	/* The other half closes the app's side of the pad first: the kernel can
	 * stall a destroy that races with an app closing a node that has effects. */
	if (!fgets(line, sizeof line, stdin)) die("no go on stdin");
	CHECK(ioctl(fd, UI_DEV_DESTROY));
	CHECK(close(fd));
	return 0;
}
