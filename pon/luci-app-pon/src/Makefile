# SPDX-License-Identifier: GPL-2.0-only

CC ?= cc
CFLAGS ?= -O2

compile: identity-image

identity-image: identity-image.c
	$(CC) $(CPPFLAGS) $(CFLAGS) -Wall -Wextra -Werror -o $@ $<

clean:
	rm -f identity-image

install: compile
	install -d $(DESTDIR)/usr/libexec
	install -m 0755 identity-image $(DESTDIR)/usr/libexec/pon-identity-image
