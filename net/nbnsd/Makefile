# SPDX-License-Identifier: GPL-2.0-only
include $(TOPDIR)/rules.mk

PKG_NAME:=nbnsd
PKG_VERSION:=1.0
PKG_RELEASE:=1
PKG_LICENSE:=GPL-2.0-only
PKG_MAINTAINER:=coolsnowwolf <coolsnowwolf@gmail.com>
PKG_BUILD_FLAGS:=gc-sections

include $(INCLUDE_DIR)/package.mk

TARGET_LDFLAGS += -Wl,--as-needed

define Package/nbnsd
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=IP Addresses and Names
  TITLE:=Lightweight NetBIOS name responder
endef

define Package/nbnsd/description
 A standalone IPv4 NBNS responder for one local hostname, with workstation
 and file-server name queries and node status. No Samba dependency.
endef

define Build/Compile
	$(TARGET_CC) $(TARGET_CPPFLAGS) $(TARGET_CFLAGS) -std=gnu99 -Wall -Wextra \
		-o $(PKG_BUILD_DIR)/nbnsd $(PKG_BUILD_DIR)/nbnsd.c $(TARGET_LDFLAGS)
endef

define Package/nbnsd/conffiles
/etc/config/nbnsd
endef

define Package/nbnsd/install
	$(INSTALL_DIR) $(1)/usr/sbin $(1)/etc/init.d $(1)/etc/config
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/nbnsd $(1)/usr/sbin/
	$(INSTALL_BIN) ./files/nbnsd.init $(1)/etc/init.d/nbnsd
	$(INSTALL_CONF) ./files/nbnsd.config $(1)/etc/config/nbnsd
endef

$(eval $(call BuildPackage,nbnsd))
