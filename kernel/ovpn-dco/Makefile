#
# Copyright (C) 2021 Jianhui Zhao <zhaojh329@gmail.com>
#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#

include $(TOPDIR)/rules.mk
include $(INCLUDE_DIR)/kernel.mk

PKG_NAME:=ovpn-dco
PKG_VERSION:=0.2.20241216
PKG_RELEASE:=1

PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL=https://codeload.github.com/OpenVPN/ovpn-dco/tar.gz/v$(PKG_VERSION)?
PKG_HASH:=0b30b8973b362b80a05f278ae217f01a2a2417e16e9c68c486960275a55306cc

PKG_MAINTAINER:=Jianhui Zhao <zhaojh329@gmail.com>
PKG_LICENSE:=GPL-2.0-only


include $(INCLUDE_DIR)/package.mk

ifeq ($(call kernel_patchver_ge,6.16),1)
OVPN_KCONFIG:= \
	CONFIG_OVPN \
	CONFIG_STREAM_PARSER=y \
	CONFIG_CRYPTO_CHACHA20POLY1305=y
OVPN_FILES:=$(LINUX_DIR)/drivers/net/ovpn/ovpn.ko
OVPN_AUTOLOAD:=$(call AutoLoad,30,ovpn)
else
OVPN_FILES:=$(PKG_BUILD_DIR)/drivers/net/ovpn-dco/ovpn-dco-v2.ko
OVPN_AUTOLOAD:=$(call AutoLoad,30,ovpn-dco-v2)
endif

define KernelPackage/ovpn-dco-v2
  SUBMENU:=Network Support
  TITLE:=OpenVPN data channel offload
  DEPENDS:=+kmod-crypto-aead +kmod-udptunnel4 +IPV6:kmod-udptunnel6
  KCONFIG:=$(OVPN_KCONFIG)
  FILES:=$(OVPN_FILES)
  AUTOLOAD:=$(OVPN_AUTOLOAD)
endef

define KernelPackage/ovpn-dco-v2/description
  This module enhances the performance of the OpenVPN userspace software
  by offloading the data channel processing to kernelspace.
endef

ifneq ($(call kernel_patchver_ge,6.16),1)
NOSTDINC_FLAGS += \
	$(KERNEL_NOSTDINC_FLAGS) \
	-I$(PKG_BUILD_DIR)/include \
	-I$(PKG_BUILD_DIR)/compat-include \
	-include $(PKG_BUILD_DIR)/linux-compat.h

EXTRA_KCONFIG:= \
	CONFIG_OVPN_DCO_V2=m

PKG_EXTMOD_SUBDIRS = drivers/net/ovpn-dco

MAKE_OPTS:= \
	$(KERNEL_MAKE_FLAGS) \
	M="$(PKG_BUILD_DIR)/drivers/net/ovpn-dco" \
	NOSTDINC_FLAGS="$(NOSTDINC_FLAGS)" \
	$(EXTRA_KCONFIG)

define Build/Compile
	$(MAKE) -C "$(LINUX_DIR)" \
		$(MAKE_OPTS) \
		modules
endef
else
define Build/Compile
endef
endif

$(eval $(call KernelPackage,ovpn-dco-v2))
