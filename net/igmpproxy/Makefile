#
# Copyright (C) 2006-2011 OpenWrt.org
#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=igmpproxy
PKG_VERSION:=0.4
PKG_RELEASE:=1

PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://github.com/pali/igmpproxy/releases/download/${PKG_VERSION}/
PKG_HASH:=afa4b75a823b82f71ce99f33eae4e8136b906ae8a5ede5caaad93bac38cdae24
PKG_MAINTAINER:=Felix Fietkau <nbd@nbd.name>

include $(INCLUDE_DIR)/package.mk

PKG_FIXUP:=autoreconf
PKG_LICENSE:=GPL-2.0+

define Package/igmpproxy
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Routing and Redirection
  DEPENDS:=+USE_GLIBC:librt
  TITLE:=Multicast Routing Daemon
  URL:=http://sourceforge.net/projects/igmpproxy
endef

define Package/igmpproxy/description
	IGMPproxy is a simple dynamic Multicast Routing Daemon using
	only IGMP signalling (Internet Group Management Protocol).
endef

define Package/igmpproxy/config
  if PACKAGE_igmpproxy
    if PACKAGE_kmod-qca-mcs
      config PACKAGE_IGMPROXY_QCA_MCS_SUPPORT
        bool "Enable QCA Multicast Snooping Module Support"
        default y
        help
          This option enables support for the QCA Multicast Snooping module.
          Enabling this will enable multicast traffic to be accelerated by
          the QCA NSS ECM module.
    endif
  endif
endef

define Package/igmpproxy/conffiles
/etc/config/igmpproxy
endef

TARGET_CFLAGS += -Dlog=igmpproxy_log

ifneq ($(CONFIG_PACKAGE_IGMPROXY_QCA_MCS_SUPPORT),)
  CONFIG_SUFFIX=.qca-mcs
  TARGET_CFLAGS += -DIGMPPROXY_QCA_MCS_SUPPORT
endif

define Build/Compile
	cd $(PKG_BUILD_DIR) && aclocal
	$(MAKE) -C $(PKG_BUILD_DIR)/src \
		CC="$(TARGET_CC)" \
		CFLAGS="$(TARGET_CFLAGS) -std=gnu99"
endef

define Package/igmpproxy/install
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/igmpproxy.config$(CONFIG_SUFFIX) $(1)/etc/config/igmpproxy
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/igmpproxy.init$(CONFIG_SUFFIX) $(1)/etc/init.d/igmpproxy
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/src/igmpproxy $(1)/usr/sbin/
endef

$(eval $(call BuildPackage,igmpproxy))
