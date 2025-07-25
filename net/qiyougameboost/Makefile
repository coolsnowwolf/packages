# SPDX-License-Identifier: GPL-2.0-only
#

include $(TOPDIR)/rules.mk

PKG_NAME:=qiyougameboost
PKG_VERSION:=1.2.0
PKG_RELEASE:=1

PKG_SOURCE_SUBDIR:=$(PKG_NAME)_$(PKG_VERSION)
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_SOURCE_SUBDIR)

PKG_LICENSE:=Proprietary
PKG_MAINTAINER:=qiyouacc, Internet1235

include $(INCLUDE_DIR)/package.mk

define Package/$(PKG_NAME)
  SECTION:=net
  CATEGORY:=Network
  TITLE:=QiYou Game Boost
  DEPENDS:=+kmod-tun +curl
  URL:=https://www.qiyou.cn
endef

define Package/$(PKG_NAME)/description
  Play console games online with less lag and more stability 
  — now supporting PS, Switch, Xbox, PC, and mobile.
endef

define Build/Prepare
	rm -f $(STAMP_BUILT)
endef

define Build/Compile
endef

define Package/$(PKG_NAME)/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) files/qiyougameboost.sh $(1)/usr/bin/qiyougameboost.sh

	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) files/qiyougameboost.init $(1)/etc/init.d/qiyougameboost

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) files/qiyougameboost.config $(1)/etc/config/qiyougameboost
endef

$(eval $(call BuildPackage,$(PKG_NAME)))
