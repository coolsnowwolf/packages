#
# Copyright (C) 2017-2024
#
# This is free software, licensed under the GNU General Public License v2.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=clouddrive2
PKG_VERSION:=0.9.16
PKG_RELEASE=1

ifeq ($(ARCH),aarch64)
	PKG_ARCH:=aarch64
	PKG_HASH:=38ea4b87454fc9c7c47266982e56b28ab810a21650c757fa2c126045945a2d39
else ifeq ($(ARCH),arm)
	PKG_ARCH:=armv7
	PKG_HASH:=7543a59822c25715eeaf1f1b780c89f1d98e63725451a1adb11d4adba35fcea6
else ifeq ($(ARCH),x86_64)
	PKG_ARCH:=x86_64
	PKG_HASH:=08d0749d4316a42172ff8e2678767dab8c2a69038adca815e1cc2565e92efa8e
endif

include $(INCLUDE_DIR)/package.mk

define Package/clouddrive2
  SECTION:=net
  CATEGORY:=Network
  DEPENDS:=@(arm||aarch64||x86_64) +fuse-utils
  TITLE:=CloudDrive2
endef

define Package/clouddrive2/description
  CloudDrive2 is a cloud storage mounting tool for OpenWrt.
endef

PKG_SOURCE:=clouddrive-2-linux-$(PKG_ARCH)-$(PKG_VERSION).tgz
PKG_SOURCE_URL:=https://github.com/cloud-fs/cloud-fs.github.io/releases/download/v$(PKG_VERSION)

define Build/Prepare
	$(call Build/Prepare/Default)
	tar -xzvf $(DL_DIR)/clouddrive-2-linux-$(PKG_ARCH)-$(PKG_VERSION).tgz -C $(PKG_BUILD_DIR)/ --strip-components=1
endef

define Build/Compile
endef

define Package/clouddrive2/install
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) $(CURDIR)/files/clouddrive2.init $(1)/etc/init.d/clouddrive2
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) $(CURDIR)/files/clouddrive2.config $(1)/etc/config/clouddrive2
	$(INSTALL_DIR) $(1)/usr/share/clouddrive2
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/clouddrive-2-linux-$(PKG_ARCH)-$(PKG_VERSION)/clouddrive $(1)/usr/share/clouddrive2/
	cp -rf $(PKG_BUILD_DIR)/clouddrive-2-linux-$(PKG_ARCH)-$(PKG_VERSION)/wwwroot $(1)/usr/share/clouddrive2/
endef

$(eval $(call BuildPackage,clouddrive2))
