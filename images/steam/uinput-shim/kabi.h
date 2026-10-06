/* The kernel's uinput and evdev structs and request numbers as the calling
 * program sees them, written out rather than taken from the linux headers so that an
 * i386 and an x86-64 build can't disagree with what the caller (Steam's 32-bit
 * client, or the tests) was compiled against. Sizes differ between the two
 * bitnesses in input_event (long timestamps), ff_effect (its union ends in a
 * pointer) and uinput_ff_upload (two ff_effects); the static asserts say
 * which. */
#ifndef CHA_UINPUT_KABI_H
#define CHA_UINPUT_KABI_H

#include <stddef.h>
#include <stdint.h>

struct k_input_event {
	long tv_sec;
	long tv_usec;
	uint16_t type;
	uint16_t code;
	int32_t value;
};

struct k_input_id {
	uint16_t bustype, vendor, product, version;
};

struct k_uinput_setup {
	struct k_input_id id;
	char name[80];
	uint32_t ff_effects_max;
};

struct k_absinfo {
	int32_t value, minimum, maximum, fuzz, flat, resolution;
};

struct k_uinput_abs_setup {
	uint16_t code;
	struct k_absinfo absinfo;
};

/* The legacy API: a write() of this before UI_DEV_CREATE. */
struct k_uinput_user_dev {
	char name[80];
	struct k_input_id id;
	uint32_t ff_effects_max;
	int32_t absmax[64], absmin[64], absfuzz[64], absflat[64];
};

struct k_ff_effect {
	uint16_t type;
	int16_t id;
	uint16_t direction;
	uint16_t trigger_button, trigger_interval;
	uint16_t replay_length, replay_delay;
	union {
		struct {
			uint16_t waveform, period;
			int16_t magnitude, offset;
			uint16_t phase;
			uint16_t envelope[4];
			uint32_t custom_len;
			int16_t *custom_data;
		} periodic;
		uint8_t raw[24];
	} u;
};

struct k_uinput_ff_upload {
	uint32_t request_id;
	int32_t retval;
	struct k_ff_effect effect, old;
};

struct k_uinput_ff_erase {
	uint32_t request_id;
	int32_t retval;
	uint32_t effect_id;
};

#define K_BITS64 (sizeof(long) == 8)
_Static_assert(sizeof(struct k_input_event) == (K_BITS64 ? 24 : 16), "input_event");
_Static_assert(sizeof(struct k_uinput_setup) == 92, "uinput_setup");
_Static_assert(sizeof(struct k_uinput_abs_setup) == 28, "uinput_abs_setup");
_Static_assert(sizeof(struct k_uinput_user_dev) == 1116, "uinput_user_dev");
_Static_assert(sizeof(struct k_ff_effect) == (K_BITS64 ? 48 : 44), "ff_effect");
_Static_assert(offsetof(struct k_ff_effect, u) == 16, "ff_effect union");
_Static_assert(sizeof(struct k_uinput_ff_upload) == (K_BITS64 ? 104 : 96), "uinput_ff_upload");
_Static_assert(sizeof(struct k_uinput_ff_erase) == 12, "uinput_ff_erase");

/* _IOC(dir, 'U', nr, size): dir 1 is write, 2 read, 3 both. Requests are
 * recognised by 'U' and nr alone (UI_SET_PHYS's size, for one, is a pointer's). */
#define K_IOC(dir, type, nr, size) \
	(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | ((unsigned long)(type) << 8) | (nr))
#define K_IOC_NR(req) ((unsigned)((req) & 0xff))
#define K_IOC_TYPE(req) ((unsigned)(((req) >> 8) & 0xff))
#define K_IOC_SIZE(req) ((unsigned)(((req) >> 16) & 0x3fff))
#define K_IOC_DIR(req) ((unsigned)(((req) >> 30) & 3))

#define K_UINPUT_TYPE 'U'
#define NR_UI_DEV_CREATE 1
#define NR_UI_DEV_DESTROY 2
#define NR_UI_DEV_SETUP 3
#define NR_UI_ABS_SETUP 4
#define NR_UI_GET_SYSNAME 44
#define NR_UI_GET_VERSION 45
#define NR_UI_SET_EVBIT 100
#define NR_UI_SET_KEYBIT 101
#define NR_UI_SET_RELBIT 102
#define NR_UI_SET_ABSBIT 103
#define NR_UI_SET_MSCBIT 104
#define NR_UI_SET_LEDBIT 105
#define NR_UI_SET_SNDBIT 106
#define NR_UI_SET_FFBIT 107
#define NR_UI_SET_PHYS 108
#define NR_UI_SET_SWBIT 109
#define NR_UI_SET_PROPBIT 110
#define NR_UI_BEGIN_FF_UPLOAD 200
#define NR_UI_END_FF_UPLOAD 201
#define NR_UI_BEGIN_FF_ERASE 202
#define NR_UI_END_FF_ERASE 203

/* The full numbers, for programs that make the calls (the tests). */
#define UI_DEV_CREATE K_IOC(0, 'U', NR_UI_DEV_CREATE, 0)
#define UI_DEV_DESTROY K_IOC(0, 'U', NR_UI_DEV_DESTROY, 0)
#define UI_DEV_SETUP K_IOC(1, 'U', NR_UI_DEV_SETUP, sizeof(struct k_uinput_setup))
#define UI_ABS_SETUP K_IOC(1, 'U', NR_UI_ABS_SETUP, sizeof(struct k_uinput_abs_setup))
#define UI_GET_SYSNAME(len) K_IOC(2, 'U', NR_UI_GET_SYSNAME, len)
#define UI_GET_VERSION K_IOC(2, 'U', NR_UI_GET_VERSION, sizeof(unsigned int))
#define UI_SET_EVBIT K_IOC(1, 'U', NR_UI_SET_EVBIT, sizeof(int))
#define UI_SET_KEYBIT K_IOC(1, 'U', NR_UI_SET_KEYBIT, sizeof(int))
#define UI_SET_ABSBIT K_IOC(1, 'U', NR_UI_SET_ABSBIT, sizeof(int))
#define UI_SET_FFBIT K_IOC(1, 'U', NR_UI_SET_FFBIT, sizeof(int))
#define UI_SET_PHYS K_IOC(1, 'U', NR_UI_SET_PHYS, sizeof(char *))
#define UI_BEGIN_FF_UPLOAD K_IOC(3, 'U', NR_UI_BEGIN_FF_UPLOAD, sizeof(struct k_uinput_ff_upload))
#define UI_END_FF_UPLOAD K_IOC(1, 'U', NR_UI_END_FF_UPLOAD, sizeof(struct k_uinput_ff_upload))
#define UI_BEGIN_FF_ERASE K_IOC(3, 'U', NR_UI_BEGIN_FF_ERASE, sizeof(struct k_uinput_ff_erase))
#define UI_END_FF_ERASE K_IOC(1, 'U', NR_UI_END_FF_ERASE, sizeof(struct k_uinput_ff_erase))

#define K_EV_SYN 0x00
#define K_EV_KEY 0x01
#define K_EV_ABS 0x03
#define K_EV_FF 0x15
#define K_EV_UINPUT 0x0101
#define K_UI_FF_UPLOAD 1
#define K_UI_FF_ERASE 2
#define K_BTN_SOUTH 0x130
#define K_FF_RUMBLE 0x50
#define K_FF_PERIODIC 0x51
#define K_FF_SINE 0x5a
#define K_FF_GAIN 0x60

#endif
