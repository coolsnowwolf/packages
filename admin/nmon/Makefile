# SPDX-License-Identifier: GPL-2.0-or-later

include $(TOPDIR)/rules.mk

PKG_NAME:=nmon
PKG_VERSION:=16n
PKG_RELEASE:=1

PKG_SOURCE:=lmon16n.c
PKG_SOURCE_URL:=@SF/nmon
PKG_HASH:=c0012cc2d925dee940c37ceae297abac64ba5a5c30e575e7418b04028613f5f2

include $(INCLUDE_DIR)/package.mk

define Package/nmon
  SECTION:=admin
  CATEGORY:=Administration
  URL:=https://nmon.sourceforge.net/
  TITLE:=Nigel's performance Monitor
  DEPENDS:=@(arm||aarch64||powerpc||i386||x86_64) \
	+libc +libncurses +lscpu
endef

define Package/nmon/description	
nmon is short for Nigel's performance Monitor for Linux
endef

define Build/Prepare
	$(CP) ./src/* $(PKG_BUILD_DIR)
	$(CP) $(DL_DIR)/$(PKG_SOURCE) $(PKG_BUILD_DIR)
	$(Build/Patch)
endef

define Package/nmon/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/nmon $(1)/usr/bin/
endef

$(eval $(call BuildPackage,nmon))
