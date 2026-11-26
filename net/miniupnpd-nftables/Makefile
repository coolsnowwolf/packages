#
# Copyright (C) 2006-2014 OpenWrt.org
#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=miniupnpd
PKG_VERSION:=2.3.11
PKG_RELEASE:=1

PKG_SOURCE_URL:=https://github.com/miniupnp/miniupnp/releases/download/miniupnpd_$(subst .,_,$(PKG_VERSION))
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_HASH:=91994b127da735b2c97f19992e34420648c0e8c4ace8a4bcb0596e7685c48678

PKG_MAINTAINER:=Alexandru Ardelean <ardeleanalex@gmail.com>
PKG_LICENSE:=BSD-3-Clause
PKG_LICENSE_FILES:=LICENSE
PKG_CPE_ID:=cpe:/a:miniupnp_project:miniupnpd

PKG_INSTALL:=1
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(INCLUDE_DIR)/version.mk

define Package/miniupnpd-nftables
  SECTION:=net
  CATEGORY:=Network
  DEPENDS:= \
	+flock \
	+libcap-ng \
	+libmnl \
	+libnftnl \
	+libuuid
  PROVIDES:=miniupnpd
  TITLE:=Lightweight UPnP IGD & PCP/NAT-PMP daemon (nftables)
  SUBMENU:=Firewall
  URL:=https://miniupnp.tuxfamily.org/
  VARIANT:=nftables
endef

define Package/miniupnpd-nftables/conffiles
/etc/config/upnpd
endef

define Build/Prepare
	$(call Build/Prepare/Default)
	echo "$(VERSION_NUMBER)" | tr '() ' '_' >$(PKG_BUILD_DIR)/os.openwrt
endef

CONFIGURE_ARGS = \
	--disable-fork \
	--disable-pppconn \
	--firewall=$(BUILD_VARIANT) \
	--igd2 \
	$(if $(CONFIG_IPV6),--ipv6) \
	--leasefile \
	--portinuse \
	--regex \
	--vendorcfg

TARGET_CFLAGS += $(FPIC)
TARGET_LDFLAGS += -Wl,--gc-sections,--as-needed

define Package/miniupnpd-nftables/install
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_DIR) $(1)/etc/hotplug.d/iface
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/usr/sbin/miniupnpd $(1)/usr/sbin/miniupnpd
	$(INSTALL_BIN) ./files/miniupnpd.init $(1)/etc/init.d/miniupnpd
	$(INSTALL_CONF) ./files/upnpd.config $(1)/etc/config/upnpd
	$(INSTALL_DATA) ./files/miniupnpd.hotplug $(1)/etc/hotplug.d/iface/50-miniupnpd
	$(INSTALL_DIR) $(1)/etc/uci-defaults $(1)/usr/share/miniupnpd
	$(INSTALL_BIN) ./files/miniupnpd.defaults $(1)/etc/uci-defaults/99-miniupnpd
	$(INSTALL_BIN) ./files/firewall4.include $(1)/usr/share/miniupnpd/firewall4.include
	$(INSTALL_DIR) $(1)/usr/share/nftables.d
	$(CP) ./files/nftables.d/* $(1)/usr/share/nftables.d/
endef

$(eval $(call BuildPackage,miniupnpd-nftables))
