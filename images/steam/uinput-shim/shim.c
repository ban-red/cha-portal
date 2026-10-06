/* libcha-uinput: a fake /dev/uinput for Steam's client.
 *
 * Steam makes the virtual Xbox pad it hands games through /dev/uinput, and the
 * app's container has none (with it, a game could make keyboards and mice on
 * the node's kernel). Loaded by LD_PRELOAD into Steam's processes
 * (start-steam), this library catches libc's open() of /dev/uinput (and
 * /dev/input/uinput) and returns a connection to the streamer's broker
 * (/run/cha/uinput.sock, crates/cha-streamer/src/uinput_broker.rs) instead.
 * Every ioctl, write and read on that descriptor is turned into a packet of
 * proto.h's protocol (ABI-neutral: this file is built for i386, which is what
 * Steam's client is, and x86-64, which its helpers load harmlessly) and the
 * broker validates it and makes the real device. Anything else passes through.
 *
 * Both uinput APIs are served (UI_DEV_SETUP/UI_ABS_SETUP, and the legacy
 * write of a uinput_user_dev), and every call on a shim descriptor is logged to
 * /run/cha/uinput-shim.log (capped), because what Steam really calls was
 * traced only as far as its open().
 *
 * Env, for the tests: CHA_UINPUT_SOCK, CHA_UINPUT_LOG. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

#include "kabi.h"
#include "proto.h"

#define LOG_PATH "/run/cha/uinput-shim.log"
#define LOG_CAP (1024 * 1024)
#define MAX_SLOTS 16
#define QUEUE_LEN 32
#define PUSH_MAX 96
#define MAX_FF 8
#define RPC_TIMEOUT_MS 5000
/* Event writes logged in full per descriptor (the log is for learning what
 * Steam does, not for its input stream). */
#define LOG_WRITES 40

/* ---- the libc functions we stand in for ---- */

typedef int (*open_fn)(const char *, int, ...);
typedef int (*openat_fn)(int, const char *, int, ...);
typedef int (*open2_fn)(const char *, int);
typedef int (*openat2_fn)(int, const char *, int);
typedef int (*ioctl_fn)(int, unsigned long, ...);
typedef ssize_t (*read_fn)(int, void *, size_t);
typedef ssize_t (*write_fn)(int, const void *, size_t);
typedef int (*close_fn)(int);
typedef ssize_t (*read_chk_fn)(int, void *, size_t, size_t);

#define REAL(type, name) \
	static type real_##name; \
	static type get_##name(void) { \
		if (!real_##name) real_##name = (type)dlsym(RTLD_NEXT, #name); \
		return real_##name; \
	}

REAL(open_fn, open)
REAL(open_fn, open64)
REAL(openat_fn, openat)
REAL(openat_fn, openat64)
REAL(open2_fn, __open_2)
REAL(open2_fn, __open64_2)
REAL(openat2_fn, __openat_2)
REAL(openat2_fn, __openat64_2)
REAL(ioctl_fn, ioctl)
REAL(read_fn, read)
REAL(write_fn, write)
REAL(close_fn, close)
REAL(read_chk_fn, __read_chk)

/* ---- the log ---- */

static pthread_mutex_t log_lock = PTHREAD_MUTEX_INITIALIZER;
static int log_fd = -2; /* -2: not tried; -1: none */

static void logf_(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void logf_(const char *fmt, ...)
{
	char line[512];
	struct timespec ts;
	int n, saved = errno;
	va_list ap;

	pthread_mutex_lock(&log_lock);
	if (log_fd == -2) {
		const char *path = getenv("CHA_UINPUT_LOG");
		open_fn o = get_open();
		log_fd = o ? o(path ? path : LOG_PATH, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0644) : -1;
	}
	if (log_fd >= 0) {
		struct stat st;
		if (fstat(log_fd, &st) == 0 && st.st_size >= LOG_CAP) {
			pthread_mutex_unlock(&log_lock);
			errno = saved;
			return;
		}
		clock_gettime(CLOCK_MONOTONIC, &ts);
		n = snprintf(line, sizeof line, "%ld.%03ld [%d] ", (long)ts.tv_sec, ts.tv_nsec / 1000000, (int)getpid());
		va_start(ap, fmt);
		n += vsnprintf(line + n, sizeof line - n - 1, fmt, ap);
		va_end(ap);
		if (n > (int)sizeof line - 2) n = sizeof line - 2;
		line[n++] = '\n';
		write_fn w = get_write();
		if (w) (void)w(log_fd, line, n);
	}
	pthread_mutex_unlock(&log_lock);
	errno = saved;
}

/* ---- the descriptors we own ---- */

struct pkt {
	uint16_t len;
	uint8_t data[PUSH_MAX];
};

/* An FF request the broker pushed and the client hasn't taken yet. */
struct ff_req {
	int used;
	int erase;
	uint32_t id;
	uint32_t effect_id;
	uint8_t effect[CHA_EFFECT_LEN];
	uint8_t old[CHA_EFFECT_LEN];
};

struct slot {
	int fd; /* -1: free */
	pthread_mutex_t lock;
	int created;
	int eof;
	uint64_t absmask; /* abs codes set, for the legacy API's arrays */
	struct pkt queue[QUEUE_LEN];
	int q_head, q_count;
	struct ff_req ff[MAX_FF];
	int writes_logged;
};

static struct slot slots[MAX_SLOTS] = {[0 ... MAX_SLOTS - 1] = {.fd = -1, .lock = PTHREAD_MUTEX_INITIALIZER}};
static pthread_mutex_t table_lock = PTHREAD_MUTEX_INITIALIZER;

/* Looked up on every read, write, ioctl and close in Steam's processes: no
 * lock, and nothing at all while no descriptor is ours. */
static int slots_in_use;

static struct slot *slot_for(int fd)
{
	int i;

	if (fd < 0 || !__atomic_load_n(&slots_in_use, __ATOMIC_RELAXED)) return NULL;
	for (i = 0; i < MAX_SLOTS; i++)
		if (__atomic_load_n(&slots[i].fd, __ATOMIC_RELAXED) == fd) return &slots[i];
	return NULL;
}

static struct slot *slot_alloc(int fd)
{
	struct slot *s = NULL;
	int i;

	pthread_mutex_lock(&table_lock);
	for (i = 0; i < MAX_SLOTS; i++)
		if (slots[i].fd == -1) {
			s = &slots[i];
			s->created = 0;
			s->eof = 0;
			s->absmask = 0;
			s->q_head = s->q_count = 0;
			s->writes_logged = 0;
			memset(s->ff, 0, sizeof s->ff);
			__atomic_add_fetch(&slots_in_use, 1, __ATOMIC_SEQ_CST);
			__atomic_store_n(&s->fd, fd, __ATOMIC_SEQ_CST);
			break;
		}
	pthread_mutex_unlock(&table_lock);
	return s;
}

static void slot_free(struct slot *s)
{
	pthread_mutex_lock(&table_lock);
	__atomic_store_n(&s->fd, -1, __ATOMIC_SEQ_CST);
	__atomic_sub_fetch(&slots_in_use, 1, __ATOMIC_SEQ_CST);
	pthread_mutex_unlock(&table_lock);
}

static int is_uinput_path(const char *path)
{
	return path && (strcmp(path, "/dev/uinput") == 0 || strcmp(path, "/dev/input/uinput") == 0);
}

/* ---- talking to the broker ---- */

static void queue_push(struct slot *s, const uint8_t *p, size_t n)
{
	struct pkt *q;

	if (n > PUSH_MAX || s->q_count == QUEUE_LEN) {
		logf_("dropped a pushed packet (op %#x, %zu bytes, %d queued)", n ? p[0] : 0, n, s->q_count);
		return;
	}
	q = &s->queue[(s->q_head + s->q_count++) % QUEUE_LEN];
	q->len = (uint16_t)n;
	memcpy(q->data, p, n);
}

/* Sends one packet, waiting if the socket is full. */
static int send_packet(struct slot *s, const uint8_t *p, size_t n)
{
	for (;;) {
		ssize_t r = send(s->fd, p, n, MSG_NOSIGNAL | MSG_DONTWAIT);
		if (r == (ssize_t)n) return 0;
		if (r < 0 && errno == EINTR) continue;
		if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
			struct pollfd pf = {.fd = s->fd, .events = POLLOUT};
			if (poll(&pf, 1, RPC_TIMEOUT_MS) <= 0) {
				errno = ETIMEDOUT;
				return -1;
			}
			continue;
		}
		if (r >= 0) errno = EIO;
		if (errno == EPIPE || errno == ECONNRESET) errno = ENODEV;
		return -1;
	}
}

/* One request and its reply, which the caller of an ioctl waits for even on a
 * non-blocking descriptor. What the broker pushes meanwhile (FF requests, play
 * events) is queued for read(). Called with s->lock held. Returns the broker's
 * errno (0 for success) or -1 with errno set when the exchange itself failed;
 * a payload goes to `payload` (at most `cap` bytes), its size to `*plen`. */
static int rpc(struct slot *s, const uint8_t *req, size_t n, uint8_t *payload, size_t cap, size_t *plen)
{
	uint8_t buf[CHA_MAX_PACKET];

	if (plen) *plen = 0;
	if (s->eof) {
		errno = ENODEV;
		return -1;
	}
	if (send_packet(s, req, n) < 0) return -1;
	for (;;) {
		struct pollfd pf = {.fd = s->fd, .events = POLLIN};
		ssize_t r;
		int pr = poll(&pf, 1, RPC_TIMEOUT_MS);
		if (pr < 0 && errno == EINTR) continue;
		if (pr <= 0) {
			errno = ETIMEDOUT;
			return -1;
		}
		r = recv(s->fd, buf, sizeof buf, MSG_DONTWAIT);
		if (r < 0 && (errno == EAGAIN || errno == EINTR)) continue;
		if (r <= 0) {
			s->eof = 1;
			errno = ENODEV;
			return -1;
		}
		if (buf[0] == OP_REPLY && r >= 5) {
			size_t body = (size_t)r - 5;
			if (payload && body) {
				if (body > cap) body = cap;
				memcpy(payload, buf + 5, body);
				if (plen) *plen = body;
			}
			return (int)get32(buf + 1);
		}
		queue_push(s, buf, (size_t)r);
	}
}

/* An rpc result as a syscall's: 0, or -1 with errno. */
static int rpc_result(int rc)
{
	if (rc > 0) {
		errno = rc;
		return -1;
	}
	return rc;
}

/* ---- ff_effect between the caller's layout and the wire's ---- */

static void effect_from_wire(struct k_ff_effect *e, const uint8_t *w)
{
	memset(e, 0, sizeof *e);
	e->type = get16(w);
	e->id = (int16_t)get16(w + 2);
	e->direction = get16(w + 4);
	e->trigger_button = get16(w + 6);
	e->trigger_interval = get16(w + 8);
	e->replay_length = get16(w + 10);
	e->replay_delay = get16(w + 12);
	memcpy(e->u.raw, w + 16, 24);
}

/* ---- read() ---- */

/* Turns the queued packet or one from the socket into an event for the
 * caller. Returns 0 if the packet isn't one that makes an event. */
static int packet_to_event(struct slot *s, const uint8_t *p, size_t n, struct k_input_event *ev)
{
	memset(ev, 0, sizeof *ev);
	if (p[0] == OP_EVENT && n == 1 + CHA_EVENT_LEN) {
		ev->type = get16(p + 1);
		ev->code = get16(p + 3);
		ev->value = (int32_t)get32(p + 5);
		return 1;
	}
	if ((p[0] == OP_FF_UPLOAD && n == 1 + 4 + 2 * CHA_EFFECT_LEN) || (p[0] == OP_FF_ERASE && n == 1 + 8)) {
		int i, erase = p[0] == OP_FF_ERASE;
		struct ff_req *r = NULL;
		for (i = 0; i < MAX_FF; i++)
			if (!s->ff[i].used) {
				r = &s->ff[i];
				break;
			}
		if (!r) {
			logf_("no room for a force feedback request");
			return 0;
		}
		r->used = 1;
		r->erase = erase;
		r->id = get32(p + 1);
		if (erase) {
			r->effect_id = get32(p + 5);
		} else {
			memcpy(r->effect, p + 5, CHA_EFFECT_LEN);
			memcpy(r->old, p + 5 + CHA_EFFECT_LEN, CHA_EFFECT_LEN);
		}
		ev->type = K_EV_UINPUT;
		ev->code = erase ? K_UI_FF_ERASE : K_UI_FF_UPLOAD;
		ev->value = (int32_t)r->id;
		return 1;
	}
	logf_("ignored a pushed packet (op %#x, %zu bytes)", p[0], n);
	return 0;
}

/* Fills `buf` with events from what's queued and what the socket has now. */
static size_t drain_events(struct slot *s, uint8_t *buf, size_t len)
{
	size_t out = 0;

	while (out + sizeof(struct k_input_event) <= len) {
		struct k_input_event ev;
		uint8_t raw[CHA_MAX_PACKET];
		const uint8_t *p;
		size_t n;
		ssize_t r;

		if (s->q_count) {
			struct pkt *q = &s->queue[s->q_head];
			memcpy(raw, q->data, q->len);
			n = q->len;
			s->q_head = (s->q_head + 1) % QUEUE_LEN;
			s->q_count--;
		} else {
			r = recv(s->fd, raw, sizeof raw, MSG_DONTWAIT);
			if (r == 0) {
				s->eof = 1;
				break;
			}
			if (r < 0) break;
			n = (size_t)r;
		}
		p = raw;
		if (p[0] == OP_REPLY) continue;
		if (n && packet_to_event(s, p, n, &ev)) {
			memcpy(buf + out, &ev, sizeof ev);
			out += sizeof ev;
		}
	}
	return out;
}

static ssize_t shim_read(struct slot *s, int fd, void *buf, size_t len)
{
	if (len < sizeof(struct k_input_event)) {
		errno = EINVAL;
		return -1;
	}
	for (;;) {
		size_t got;
		int nonblock = (fcntl(fd, F_GETFL) & O_NONBLOCK) != 0;
		struct pollfd pf = {.fd = fd, .events = POLLIN};

		pthread_mutex_lock(&s->lock);
		got = drain_events(s, buf, len);
		pthread_mutex_unlock(&s->lock);
		if (got) {
			struct k_input_event *ev = buf;
			logf_("read(fd %d, %zu) -> %zu bytes, first: type %#x code %#x value %d", fd, len, got, ev->type,
			      ev->code, ev->value);
			return (ssize_t)got;
		}
		if (s->eof) {
			errno = ENODEV;
			return -1;
		}
		if (nonblock) {
			errno = EAGAIN;
			return -1;
		}
		/* Waits in short turns so an ioctl in another thread, which holds the
		 * lock while it waits for its reply, is never kept waiting. */
		poll(&pf, 1, 50);
	}
}

/* ---- write() ---- */

static int send_events(struct slot *s, const struct k_input_event *ev, size_t count)
{
	uint8_t pkt[1 + CHA_MAX_EVENTS * CHA_EVENT_LEN];

	while (count) {
		size_t n = count > CHA_MAX_EVENTS ? CHA_MAX_EVENTS : count, i;
		int rc;
		pkt[0] = OP_EVENTS;
		for (i = 0; i < n; i++) {
			put16(pkt + 1 + i * CHA_EVENT_LEN, ev[i].type);
			put16(pkt + 3 + i * CHA_EVENT_LEN, ev[i].code);
			put32(pkt + 5 + i * CHA_EVENT_LEN, (uint32_t)ev[i].value);
		}
		rc = rpc(s, pkt, 1 + n * CHA_EVENT_LEN, NULL, 0, NULL);
		if (rc != 0) return rpc_result(rc);
		ev += n;
		count -= n;
	}
	return 0;
}

static int send_dev_setup(struct slot *s, const struct k_input_id *id, const char *name, uint32_t ff_max)
{
	uint8_t pkt[1 + 12 + CHA_NAME_LEN];

	pkt[0] = OP_DEV_SETUP;
	put16(pkt + 1, id->bustype);
	put16(pkt + 3, id->vendor);
	put16(pkt + 5, id->product);
	put16(pkt + 7, id->version);
	put32(pkt + 9, ff_max);
	memcpy(pkt + 13, name, CHA_NAME_LEN);
	return rpc_result(rpc(s, pkt, sizeof pkt, NULL, 0, NULL));
}

static int send_abs_setup(struct slot *s, uint16_t code, int32_t min, int32_t max, int32_t fuzz, int32_t flat,
			  int32_t res)
{
	uint8_t pkt[1 + 22];

	pkt[0] = OP_ABS_SETUP;
	put16(pkt + 1, code);
	put32(pkt + 3, (uint32_t)min);
	put32(pkt + 7, (uint32_t)max);
	put32(pkt + 11, (uint32_t)fuzz);
	put32(pkt + 15, (uint32_t)flat);
	put32(pkt + 19, (uint32_t)res);
	return rpc_result(rpc(s, pkt, sizeof pkt, NULL, 0, NULL));
}

static ssize_t shim_write(struct slot *s, int fd, const void *buf, size_t len)
{
	ssize_t ret;
	int logged;

	pthread_mutex_lock(&s->lock);
	logged = s->writes_logged < LOG_WRITES;
	if (s->created) {
		const struct k_input_event *ev = buf;
		size_t count = len / sizeof *ev;
		if (len % sizeof *ev || count == 0) {
			logf_("write(fd %d, %zu): not a whole number of %zu-byte input_events", fd, len, sizeof *ev);
			errno = EINVAL;
			ret = -1;
		} else {
			ret = send_events(s, ev, count) == 0 ? (ssize_t)len : -1;
		}
		if (logged) {
			s->writes_logged++;
			logf_("write(fd %d, %zu) events: first type %#x code %#x value %d -> %zd (errno %d)", fd, len,
			      count ? ev[0].type : 0, count ? ev[0].code : 0, count ? ev[0].value : 0, ret,
			      ret < 0 ? errno : 0);
		}
	} else if (len == sizeof(struct k_uinput_user_dev)) {
		/* The legacy API: the setup is a write before UI_DEV_CREATE. The axis
		 * ranges are arrays over every code; the broker takes them per code
		 * that was set with UI_SET_ABSBIT. */
		const struct k_uinput_user_dev *ud = buf;
		char name[CHA_NAME_LEN];
		int code, rc = 0;

		memcpy(name, ud->name, sizeof name);
		logf_("write(fd %d, %zu) legacy uinput_user_dev: name '%.79s' bus %#x vendor %#x product %#x version %#x "
		      "ff_effects_max %u",
		      fd, len, name, ud->id.bustype, ud->id.vendor, ud->id.product, ud->id.version, ud->ff_effects_max);
		rc = send_dev_setup(s, &ud->id, name, ud->ff_effects_max);
		for (code = 0; rc == 0 && code < 64; code++) {
			if (!(s->absmask >> code & 1)) continue;
			logf_("  abs %#x: min %d max %d fuzz %d flat %d", code, ud->absmin[code], ud->absmax[code],
			      ud->absfuzz[code], ud->absflat[code]);
			rc = send_abs_setup(s, (uint16_t)code, ud->absmin[code], ud->absmax[code], ud->absfuzz[code],
					    ud->absflat[code], 0);
		}
		ret = rc == 0 ? (ssize_t)len : -1;
		logf_("  -> %zd (errno %d)", ret, ret < 0 ? errno : 0);
	} else {
		logf_("write(fd %d, %zu) before UI_DEV_CREATE and not a uinput_user_dev", fd, len);
		errno = EINVAL;
		ret = -1;
	}
	pthread_mutex_unlock(&s->lock);
	return ret;
}

/* ---- ioctl() ---- */

static int set_bit(struct slot *s, uint8_t kind, unsigned long code, const char *what)
{
	uint8_t pkt[4];
	int rc;

	if (code > 0xffff) {
		errno = EINVAL;
		return -1;
	}
	if (kind == BIT_ABS && code < 64) s->absmask |= 1ull << code;
	pkt[0] = OP_SET_BIT;
	pkt[1] = kind;
	put16(pkt + 2, (uint16_t)code);
	rc = rpc(s, pkt, sizeof pkt, NULL, 0, NULL);
	logf_("ioctl %s %#lx -> %d", what, code, rc);
	return rpc_result(rc);
}

static int shim_ioctl(struct slot *s, int fd, unsigned long req, unsigned long arg)
{
	unsigned nr = K_IOC_NR(req);
	int ret;

	if (K_IOC_TYPE(req) != K_UINPUT_TYPE) {
		logf_("ioctl fd %d request %#lx (type %#x, not uinput's) -> ENOTTY", fd, req, K_IOC_TYPE(req));
		errno = ENOTTY;
		return -1;
	}
	pthread_mutex_lock(&s->lock);
	switch (nr) {
	case NR_UI_SET_EVBIT: ret = set_bit(s, BIT_EV, arg, "UI_SET_EVBIT"); break;
	case NR_UI_SET_KEYBIT: ret = set_bit(s, BIT_KEY, arg, "UI_SET_KEYBIT"); break;
	case NR_UI_SET_RELBIT: ret = set_bit(s, BIT_REL, arg, "UI_SET_RELBIT"); break;
	case NR_UI_SET_ABSBIT: ret = set_bit(s, BIT_ABS, arg, "UI_SET_ABSBIT"); break;
	case NR_UI_SET_MSCBIT: ret = set_bit(s, BIT_MSC, arg, "UI_SET_MSCBIT"); break;
	case NR_UI_SET_LEDBIT: ret = set_bit(s, BIT_LED, arg, "UI_SET_LEDBIT"); break;
	case NR_UI_SET_SNDBIT: ret = set_bit(s, BIT_SND, arg, "UI_SET_SNDBIT"); break;
	case NR_UI_SET_FFBIT: ret = set_bit(s, BIT_FF, arg, "UI_SET_FFBIT"); break;
	case NR_UI_SET_SWBIT: ret = set_bit(s, BIT_SW, arg, "UI_SET_SWBIT"); break;
	case NR_UI_SET_PROPBIT: ret = set_bit(s, BIT_PROP, arg, "UI_SET_PROPBIT"); break;
	case NR_UI_SET_PHYS:
		/* The broker names the device (cha/pad<N>) so the host's udev rule
		 * matches it; the client's is ignored. */
		logf_("ioctl UI_SET_PHYS '%.60s' (ignored)", arg ? (const char *)arg : "(null)");
		ret = 0;
		break;
	case NR_UI_GET_VERSION:
		if (arg) *(unsigned int *)arg = 5;
		logf_("ioctl UI_GET_VERSION -> 5");
		ret = 0;
		break;
	case NR_UI_DEV_SETUP: {
		const struct k_uinput_setup *u = (const struct k_uinput_setup *)arg;
		if (!u) {
			errno = EFAULT;
			ret = -1;
			break;
		}
		logf_("ioctl UI_DEV_SETUP name '%.79s' bus %#x vendor %#x product %#x version %#x ff_effects_max %u",
		      u->name, u->id.bustype, u->id.vendor, u->id.product, u->id.version, u->ff_effects_max);
		ret = send_dev_setup(s, &u->id, u->name, u->ff_effects_max);
		logf_("  -> %d (errno %d)", ret, ret < 0 ? errno : 0);
		break;
	}
	case NR_UI_ABS_SETUP: {
		const struct k_uinput_abs_setup *a = (const struct k_uinput_abs_setup *)arg;
		if (!a) {
			errno = EFAULT;
			ret = -1;
			break;
		}
		if (a->code < 64) s->absmask |= 1ull << a->code;
		logf_("ioctl UI_ABS_SETUP code %#x min %d max %d fuzz %d flat %d res %d", a->code, a->absinfo.minimum,
		      a->absinfo.maximum, a->absinfo.fuzz, a->absinfo.flat, a->absinfo.resolution);
		ret = send_abs_setup(s, a->code, a->absinfo.minimum, a->absinfo.maximum, a->absinfo.fuzz,
				     a->absinfo.flat, a->absinfo.resolution);
		logf_("  -> %d (errno %d)", ret, ret < 0 ? errno : 0);
		break;
	}
	case NR_UI_DEV_CREATE: {
		uint8_t pkt[1] = {OP_CREATE};
		ret = rpc_result(rpc(s, pkt, sizeof pkt, NULL, 0, NULL));
		if (ret == 0) s->created = 1;
		logf_("ioctl UI_DEV_CREATE -> %d (errno %d)", ret, ret < 0 ? errno : 0);
		break;
	}
	case NR_UI_DEV_DESTROY: {
		uint8_t pkt[1] = {OP_DESTROY};
		ret = rpc_result(rpc(s, pkt, sizeof pkt, NULL, 0, NULL));
		s->created = 0;
		logf_("ioctl UI_DEV_DESTROY -> %d", ret);
		break;
	}
	case NR_UI_GET_SYSNAME: {
		uint8_t pkt[1] = {OP_GET_SYSNAME}, name[128];
		size_t plen = 0, cap = K_IOC_SIZE(req);
		int rc;
		if (!arg || cap == 0) {
			errno = EINVAL;
			ret = -1;
			break;
		}
		rc = rpc(s, pkt, sizeof pkt, name, sizeof name - 1, &plen);
		if (rc != 0) {
			ret = rpc_result(rc);
		} else {
			/* As the kernel: the name and its NUL, cut to the buffer, and the
			 * length copied is the return value. */
			size_t n = plen + 1 > cap ? cap : plen + 1;
			memcpy((char *)arg, name, n - 1);
			((char *)arg)[n - 1] = 0;
			ret = (int)n;
		}
		logf_("ioctl UI_GET_SYSNAME(%zu) -> %d '%.60s'", cap, ret, ret > 0 ? (const char *)arg : "");
		break;
	}
	case NR_UI_BEGIN_FF_UPLOAD:
	case NR_UI_BEGIN_FF_ERASE: {
		int erase = nr == NR_UI_BEGIN_FF_ERASE, i;
		uint32_t id = arg ? *(uint32_t *)arg : 0;
		struct ff_req *r = NULL;
		for (i = 0; i < MAX_FF; i++)
			if (s->ff[i].used && s->ff[i].erase == erase && s->ff[i].id == id) r = &s->ff[i];
		if (!r) {
			logf_("ioctl UI_BEGIN_FF_%s request %u: none queued", erase ? "ERASE" : "UPLOAD", id);
			errno = EINVAL;
			ret = -1;
			break;
		}
		if (erase) {
			struct k_uinput_ff_erase *e = (struct k_uinput_ff_erase *)arg;
			e->retval = 0;
			e->effect_id = r->effect_id;
			logf_("ioctl UI_BEGIN_FF_ERASE request %u effect %u", id, r->effect_id);
		} else {
			struct k_uinput_ff_upload *u = (struct k_uinput_ff_upload *)arg;
			u->retval = 0;
			effect_from_wire(&u->effect, r->effect);
			effect_from_wire(&u->old, r->old);
			/* FF_RUMBLE's union starts with the strong and weak magnitudes. */
			uint16_t strong = (uint16_t)(u->effect.u.raw[0] | u->effect.u.raw[1] << 8);
			uint16_t weak = (uint16_t)(u->effect.u.raw[2] | u->effect.u.raw[3] << 8);
			logf_("ioctl UI_BEGIN_FF_UPLOAD request %u: effect type %#x id %d length %u strong %u weak %u", id,
			      u->effect.type, u->effect.id, u->effect.replay_length, strong, weak);
		}
		ret = 0;
		break;
	}
	case NR_UI_END_FF_UPLOAD:
	case NR_UI_END_FF_ERASE: {
		int erase = nr == NR_UI_END_FF_ERASE, i;
		uint32_t id = arg ? *(uint32_t *)arg : 0;
		int32_t retval = arg ? ((int32_t *)arg)[1] : -EINVAL;
		uint8_t pkt[1 + 8];
		for (i = 0; i < MAX_FF; i++)
			if (s->ff[i].used && s->ff[i].erase == erase && s->ff[i].id == id) s->ff[i].used = 0;
		pkt[0] = OP_FF_DONE;
		put32(pkt + 1, id);
		put32(pkt + 5, (uint32_t)retval);
		ret = send_packet(s, pkt, sizeof pkt);
		logf_("ioctl UI_END_FF_%s request %u retval %d -> %d", erase ? "ERASE" : "UPLOAD", id, retval, ret);
		break;
	}
	default:
		logf_("ioctl fd %d request %#lx (nr %u dir %u size %u) not known -> ENOTTY", fd, req, nr, K_IOC_DIR(req),
		      K_IOC_SIZE(req));
		errno = ENOTTY;
		ret = -1;
	}
	pthread_mutex_unlock(&s->lock);
	return ret;
}

/* ---- open() ---- */

static int shim_open(const char *path, int flags)
{
	const char *sock_path = getenv("CHA_UINPUT_SOCK");
	struct sockaddr_un addr;
	struct slot *s;
	int fd, saved;

	fd = socket(AF_UNIX, SOCK_SEQPACKET | ((flags & O_CLOEXEC) ? SOCK_CLOEXEC : 0), 0);
	if (fd >= 0) {
		memset(&addr, 0, sizeof addr);
		addr.sun_family = AF_UNIX;
		strncpy(addr.sun_path, sock_path ? sock_path : CHA_SOCKET_PATH, sizeof addr.sun_path - 1);
		if (connect(fd, (struct sockaddr *)&addr, sizeof addr) < 0) {
			saved = errno;
			close(fd);
			fd = -1;
			errno = saved;
		}
	}
	if (fd < 0) {
		/* No broker (another environment, an old streamer): as without the
		 * shim, there is no such device. */
		logf_("open(%s, %#x): no broker (%s) -> ENOENT", path, flags, strerror(errno));
		errno = ENOENT;
		return -1;
	}
	if (flags & O_NONBLOCK) fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK);
	s = slot_alloc(fd);
	if (!s) {
		close(fd);
		errno = EMFILE;
		return -1;
	}
	logf_("open(%s, %#x) -> fd %d (the broker)", path, flags, fd);
	return fd;
}

static int is_o_creat(int flags)
{
	return (flags & O_CREAT) || (flags & O_TMPFILE) == O_TMPFILE;
}

#define MODE_ARG(flags, mode) \
	do { \
		mode = 0; \
		if (is_o_creat(flags)) { \
			va_list ap_; \
			va_start(ap_, flags); \
			mode = va_arg(ap_, int); \
			va_end(ap_); \
		} \
	} while (0)

int open(const char *path, int flags, ...)
{
	int mode;
	MODE_ARG(flags, mode);
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get_open()(path, flags, mode);
}

int open64(const char *path, int flags, ...)
{
	int mode;
	MODE_ARG(flags, mode);
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get_open64()(path, flags, mode);
}

int openat(int dirfd, const char *path, int flags, ...)
{
	int mode;
	MODE_ARG(flags, mode);
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get_openat()(dirfd, path, flags, mode);
}

int openat64(int dirfd, const char *path, int flags, ...)
{
	int mode;
	MODE_ARG(flags, mode);
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get_openat64()(dirfd, path, flags, mode);
}

/* The fortified spellings glibc's headers turn open() into. */
int __open_2(const char *path, int flags)
{
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get___open_2()(path, flags);
}

int __open64_2(const char *path, int flags)
{
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get___open64_2()(path, flags);
}

int __openat_2(int dirfd, const char *path, int flags)
{
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get___openat_2()(dirfd, path, flags);
}

int __openat64_2(int dirfd, const char *path, int flags)
{
	if (is_uinput_path(path)) return shim_open(path, flags);
	return get___openat64_2()(dirfd, path, flags);
}

/* ---- the calls on our descriptors ---- */

int ioctl(int fd, unsigned long request, ...)
{
	va_list ap;
	unsigned long arg;
	struct slot *s = slot_for(fd);

	va_start(ap, request);
	arg = va_arg(ap, unsigned long);
	va_end(ap);
	if (s) return shim_ioctl(s, fd, request, arg);
	return get_ioctl()(fd, request, arg);
}

ssize_t read(int fd, void *buf, size_t count)
{
	struct slot *s = slot_for(fd);
	if (s) return shim_read(s, fd, buf, count);
	return get_read()(fd, buf, count);
}

ssize_t __read_chk(int fd, void *buf, size_t count, size_t buflen)
{
	struct slot *s = slot_for(fd);
	if (s) return shim_read(s, fd, buf, count < buflen ? count : buflen);
	return get___read_chk()(fd, buf, count, buflen);
}

ssize_t write(int fd, const void *buf, size_t count)
{
	struct slot *s = slot_for(fd);
	if (s) return shim_write(s, fd, buf, count);
	return get_write()(fd, buf, count);
}

int close(int fd)
{
	struct slot *s = slot_for(fd);
	if (s) {
		logf_("close(fd %d)", fd);
		slot_free(s);
	}
	return get_close()(fd);
}
