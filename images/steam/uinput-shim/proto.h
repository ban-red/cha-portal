/* The wire between this shim and the streamer's uinput broker. The Rust side
 * is crates/cha-streamer/src/uinput_proto.rs, which documents every layout and
 * has the tests: change both together. One SOCK_SEQPACKET connection is one
 * virtual device; a packet is an op byte and a body, integers little-endian
 * (x86 either way), and nothing depends on the caller's ABI. */
#ifndef CHA_UINPUT_PROTO_H
#define CHA_UINPUT_PROTO_H

#include <stdint.h>

#define CHA_SOCKET_PATH "/run/cha/uinput.sock"
#define CHA_MAX_PACKET 4096
#define CHA_NAME_LEN 80
#define CHA_EFFECT_LEN 40
#define CHA_EVENT_LEN 8
/* The broker refuses more events than this in one write; the shim splits. */
#define CHA_MAX_EVENTS 64

/* Shim to broker: each gets one OP_REPLY, except OP_FF_DONE. */
#define OP_SET_BIT 1     /* kind u8, code u16 */
#define OP_ABS_SETUP 2   /* code u16, min, max, fuzz, flat, res i32 */
#define OP_DEV_SETUP 3   /* bustype, vendor, product, version u16, ff_effects_max u32, name[80] */
#define OP_CREATE 4
#define OP_DESTROY 5
#define OP_GET_SYSNAME 6
#define OP_EVENTS 7      /* n x (type u16, code u16, value i32) */
#define OP_FF_DONE 8     /* request id u32, retval i32 */
/* Broker to shim. */
#define OP_REPLY 0x80    /* errno i32 (0 = ok), then a payload (GET_SYSNAME: the name, no NUL) */
#define OP_FF_UPLOAD 0x81 /* request id u32, effect[40], old effect[40] */
#define OP_FF_ERASE 0x82  /* request id u32, effect id u32 */
#define OP_EVENT 0x83     /* type u16, code u16, value i32 */

/* SET_BIT kinds. */
#define BIT_EV 0
#define BIT_KEY 1
#define BIT_ABS 2
#define BIT_FF 3
#define BIT_MSC 4
#define BIT_REL 5
#define BIT_LED 6
#define BIT_SND 7
#define BIT_SW 8
#define BIT_PROP 9

/* An effect on the wire is the kernel's struct ff_effect without the ABI:
 * type u16, id i16, direction u16, trigger button u16, trigger interval u16,
 * replay length u16, replay delay u16, 2 zero bytes, then the first 24 bytes
 * of the union (the custom data pointer isn't carried). */

static inline void put16(uint8_t *p, uint16_t v) { p[0] = v & 0xff; p[1] = v >> 8; }
static inline void put32(uint8_t *p, uint32_t v) {
	p[0] = v & 0xff; p[1] = (v >> 8) & 0xff; p[2] = (v >> 16) & 0xff; p[3] = v >> 24;
}
static inline uint16_t get16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }
static inline uint32_t get32(const uint8_t *p) {
	return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

#endif
