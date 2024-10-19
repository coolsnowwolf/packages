# SPDX-License-Identifier: MIT
#
# Copyright (c) 2017 Yu Wang <wangyucn@gmail.com>

include $(TOPDIR)/rules.mk

PKG_NAME:=UDPspeeder
PKG_VERSION:=20230206.0
PKG_RELEASE:=2

PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://codeload.github.com/wangyu-/UDPspeeder/tar.gz/$(PKG_VERSION)?
PKG_HASH:=skip

PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=Yu Wang

PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk

define Package/UDPspeeder
	SECTION:=net
	CATEGORY:=Network
	TITLE:=UDP Network Speed-Up Tool
	URL:=https://github.com/wangyu-/UDPspeeder
	DEPENDS:= +libstdcpp +librt +libatomic
endef

define Package/UDPspeeder/description
	 A Tunnel which Improves your Network Quality on a High-latency Lossy Link by using Forward Error Correction,for All Traffics(TCP/UDP/ICMP)
endef

MAKE_FLAGS += cross

define Build/Prepare
	$(PKG_UNPACK)
	sed -i 's/cc_cross=.*/cc_cross=$(TARGET_CXX)/g' $(PKG_BUILD_DIR)/makefile
	sed -i '/\gitversion/d' $(PKG_BUILD_DIR)/makefile
	echo 'const char * const gitversion = "$(PKG_VERSION)";' > $(PKG_BUILD_DIR)/git_version.h
	$(Build/Patch)
endef

define Package/UDPspeeder/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/speederv2_cross $(1)/usr/bin/udpspeeder
endef

$(eval $(call BuildPackage,UDPspeeder))
