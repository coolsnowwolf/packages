# SPDX-License-Identifier: GPL-2.0-only
#
# airoha_eth owns the QDMA rings; this module uses its PON data-path API.

include $(TOPDIR)/rules.mk
include $(INCLUDE_DIR)/kernel.mk

PKG_NAME:=airoha-xpon
PKG_RELEASE:=70
PKG_LICENSE:=GPL-2.0-only

ifneq ($(filter y,$(CONFIG_LINUX_5_4)),)
PATCH_DIR:=./patches-5.4
endif

include $(INCLUDE_DIR)/package.mk

define KernelPackage/airoha-xpon
  SUBMENU:=Network Devices
  TITLE:=Airoha AN7581/AN7583 xPON MAC support
  DEPENDS:=@(TARGET_airoha_an7581||TARGET_airoha_an7583) +kmod-airoha-pon-frontend
  FILES:=$(PKG_BUILD_DIR)/airoha-xpon.ko
  AUTOLOAD:=$(call AutoProbe,airoha-xpon)
  KCONFIG:=
endef

define KernelPackage/airoha-xpon/description
  AN7581/AN7583 xPON MAC controller with OMCI/OAM control interfaces
endef

define Build/Compile
	$(KERNEL_MAKE) M="$(PKG_BUILD_DIR)" \
		AIROHA_PON_FRONTEND_INCLUDE="$(CURDIR)/../airoha-pon-frontends/src" \
		KBUILD_EXTRA_SYMBOLS="$(PKG_BUILD_DIR)/../symvers/airoha-pon-frontends.symvers" \
		modules
endef

$(eval $(call KernelPackage,airoha-xpon))
