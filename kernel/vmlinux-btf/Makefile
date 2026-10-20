#
# Copyright (C) 2015-2024 OpenWrt.org
#
# This is free software, licensed under the GNU General Public License v3.
#

VMLINUX_BTF_DIR := $(patsubst %/,%,$(dir $(lastword $(MAKEFILE_LIST))))

include $(TOPDIR)/rules.mk
include $(INCLUDE_DIR)/kernel.mk

PKG_NAME:=vmlinux-btf
PKG_VERSION:=$(LINUX_VERSION)
PKG_RELEASE:=9

include $(INCLUDE_DIR)/package.mk

define Package/vmlinux-btf
  SECTION:=kernel
  CATEGORY:=Kernel
  TITLE:=Kernel vmlinux.btf for CO-RE compatibility
endef

define Package/vmlinux-btf/description
  Generates detached kernel BTF from target kernel headers and config,
  enabling CO-RE eBPF programs on OpenWrt without rebuilding the kernel.
endef

define Package/vmlinux-btf/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] || ln -sf vmlinux /usr/lib/debug/boot/vmlinux-$$(uname -r)
endef

define Package/vmlinux-btf/prerm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] || rm -f /usr/lib/debug/boot/vmlinux-$$(uname -r)
endef

BTF_CC ?= $(if $(TARGET_CC_NOCACHE),$(TARGET_CC_NOCACHE),$(TARGET_CROSS)gcc)
BTF_PAHOLE ?= $(firstword \
	$(wildcard $(STAGING_DIR_HOST)/bin/pahole /usr/bin/pahole /usr/local/bin/pahole) \
	$(shell command -v pahole 2>/dev/null) \
	pahole)
KERNEL_RELEASE := $(if $(wildcard $(LINUX_DIR)/include/config/kernel.release),$(shell cat $(LINUX_DIR)/include/config/kernel.release),$(LINUX_VERSION))

BTF_CFLAGS := \
	-nostdinc \
	-isystem $(shell $(BTF_CC) -print-file-name=include) \
	-D__KERNEL__ \
	-include $(LINUX_DIR)/include/linux/kconfig.h \
	$(if $(wildcard $(LINUX_DIR)/include/linux/compiler-version.h),-include $(LINUX_DIR)/include/linux/compiler-version.h) \
	$(if $(wildcard $(LINUX_DIR)/include/linux/compiler_types.h),-include $(LINUX_DIR)/include/linux/compiler_types.h) \
	-I$(LINUX_DIR)/arch/$(LINUX_KARCH)/include \
	-I$(LINUX_DIR)/arch/$(LINUX_KARCH)/include/generated \
	-I$(LINUX_DIR)/include \
	-I$(LINUX_DIR)/include/generated \
	-I$(LINUX_DIR)/arch/$(LINUX_KARCH)/include/uapi \
	-I$(LINUX_DIR)/arch/$(LINUX_KARCH)/include/generated/uapi \
	-I$(LINUX_DIR)/include/uapi \
	-I$(LINUX_DIR)/include/generated/uapi \
	-I$(LINUX_DIR) \
	$(filter -m% -march=% -mcpu=% -mtune=% -EB -EL,$(TARGET_CFLAGS)) \
	-std=gnu11 -funsigned-char -fshort-wchar -fno-common -fno-PIE \
	-O2 -g -gdwarf-4 -fno-eliminate-unused-debug-types \
	-Wno-error -Wno-unused-function -Wno-unused-variable

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) $(VMLINUX_BTF_DIR)/src/* $(PKG_BUILD_DIR)/
endef

define Build/Compile
	@if [ ! -f "$(LINUX_DIR)/include/generated/autoconf.h" ]; then \
		echo "vmlinux-btf: kernel headers not found in $(LINUX_DIR)"; \
		exit 1; \
	fi
	@if [ -z "$(BTF_PAHOLE)" ] || ! $(BTF_PAHOLE) --version >/dev/null 2>&1; then \
		echo "vmlinux-btf: pahole not found or not working (install dwarves/pahole on host)"; \
		exit 1; \
	fi

	$(BTF_CC) $(BTF_CFLAGS) -c $(PKG_BUILD_DIR)/shadow_btf_core.c -o $(PKG_BUILD_DIR)/shadow_btf_core.o
	-$(BTF_CC) $(BTF_CFLAGS) -c $(PKG_BUILD_DIR)/shadow_btf_modules.c -o $(PKG_BUILD_DIR)/shadow_btf_modules.o 2>/dev/null
	-$(BTF_CC) $(BTF_CFLAGS) -c $(PKG_BUILD_DIR)/shadow_btf_private.c -o $(PKG_BUILD_DIR)/shadow_btf_private.o 2>/dev/null
	$(TARGET_CROSS)ld -r -o $(PKG_BUILD_DIR)/shadow_btf.o $(PKG_BUILD_DIR)/shadow_btf_*.o
	if $(BTF_PAHOLE) --help 2>&1 | grep -q -- '--btf_encode_detached'; then \
		$(BTF_PAHOLE) --btf_encode_detached=$(PKG_BUILD_DIR)/vmlinux-btf $(PKG_BUILD_DIR)/shadow_btf.o; \
	else \
		$(BTF_PAHOLE) -J $(PKG_BUILD_DIR)/shadow_btf.o && \
		$(TARGET_CROSS)objcopy --dump-section .BTF=$(PKG_BUILD_DIR)/vmlinux-btf $(PKG_BUILD_DIR)/shadow_btf.o; \
	fi
	test -s $(PKG_BUILD_DIR)/vmlinux-btf
	$(BTF_PAHOLE) -C sk_buff $(PKG_BUILD_DIR)/shadow_btf.o >/dev/null
endef

define Package/vmlinux-btf/install
	$(INSTALL_DIR) $(1)/usr/lib/debug/boot
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/vmlinux-btf $(1)/usr/lib/debug/boot/vmlinux
	$(LN) vmlinux $(1)/usr/lib/debug/boot/vmlinux-$(KERNEL_RELEASE)
endef

$(eval $(call BuildPackage,vmlinux-btf))