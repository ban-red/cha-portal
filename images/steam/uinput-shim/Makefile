# libcha-uinput.so for Steam's two bitnesses (the 32-bit client loads the i386
# one; its 64-bit helpers load the other, harmlessly), laid out as the dynamic
# loader's $LIB expands (images/steam/start-steam), and the test programs.
#   make OUT=/out/opt/cha          the libraries
#   make OUT=/tmp/shim tests       also test-i386, test-amd64 and the libraries
#                                  as libcha-uinput-<arch>.so (run-tests.sh)
# Needs gcc-multilib. _FORTIFY_SOURCE would redefine open() and read().
CC ?= gcc
OUT ?= out
CFLAGS ?= -O2 -g
CFLAGS += -std=gnu11 -Wall -Wextra -Wno-unused-parameter -fPIC -U_FORTIFY_SOURCE -fno-strict-aliasing
HDRS = proto.h kabi.h

I386 = $(OUT)/lib/i386-linux-gnu/libcha-uinput.so
AMD64 = $(OUT)/lib/x86_64-linux-gnu/libcha-uinput.so

all: $(I386) $(AMD64)

$(I386): shim.c $(HDRS)
	mkdir -p $(dir $@)
	$(CC) -m32 $(CFLAGS) -shared -o $@ shim.c -ldl -lpthread

$(AMD64): shim.c $(HDRS)
	mkdir -p $(dir $@)
	$(CC) -m64 $(CFLAGS) -shared -o $@ shim.c -ldl -lpthread

tests: all $(OUT)/test-i386 $(OUT)/test-amd64 $(OUT)/libcha-uinput-i386.so $(OUT)/libcha-uinput-amd64.so

$(OUT)/test-i386: test.c $(HDRS)
	$(CC) -m32 $(CFLAGS) -o $@ test.c

$(OUT)/test-amd64: test.c $(HDRS)
	$(CC) -m64 $(CFLAGS) -o $@ test.c

$(OUT)/libcha-uinput-i386.so: $(I386)
	cp $< $@

$(OUT)/libcha-uinput-amd64.so: $(AMD64)
	cp $< $@

.PHONY: all tests
