# NOTE: link the curent working copy of the code to lmon.c for compiling
CFLAGS=-g -O3 -Wall
LDFLAGS=-lncurses -lm

ifndef ARCH
ARCH := $(shell uname -m)
endif

ifneq ($(findstring $(ARCH), arm aarch64),)
	EXTRA_CFLAGS := -DARM
else ifneq ($(findstring $(ARCH), i386 x86_64),)
	EXTRA_CFLAGS := -DX86
else ifneq ($(findstring $(ARCH), powerpc),)
	EXTRA_CFLAGS := -DPOWER
endif

nmon:
	$(CC) -o nmon lmon16n.c $(CFLAGS) $(LDFLAGS) $(EXTRA_CFLAGS)
