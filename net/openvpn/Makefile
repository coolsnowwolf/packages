
# Copyright (C) 2010-2015 OpenWrt.org
#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=openvpn

PKG_VERSION:=2.7.4
PKG_RELEASE:=4

PKG_SOURCE_URL:=\
	https://build.openvpn.net/downloads/releases/ \
	https://swupdate.openvpn.net/community/releases/
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_HASH:=18db05f3d5eee3663db1914590044e5f96ff5cd47b6e7846c6a350806c23dbce

PKG_MAINTAINER:=

PKG_INSTALL:=1
PKG_FIXUP:=autoreconf
PKG_BUILD_PARALLEL:=1
PKG_BUILD_FLAGS:=gc-sections
PKG_LICENSE:=GPL-2.0
PKG_CPE_ID:=cpe:/a:openvpn:openvpn

OPENVPN_VARIANTS:=openssl mbedtls wolfssl
PKG_CONFIG_DEPENDS:=$(foreach variant,$(OPENVPN_VARIANTS),CONFIG_OPENVPN_$(variant)_ENABLE_DCO)

# NSS is available when the main tree supplies the manager and its options.
SCAN_DEPS = $(TOPDIR)/package/qca/qca-nss-ovpn-12/openvpn-config.in
OPENVPN_NSS_CONFIG:=package/qca/qca-nss-ovpn-12/openvpn-config.in
ifneq ($(wildcard $(TOPDIR)/$(OPENVPN_NSS_CONFIG)),)
OPENVPN_NSS_ENABLED=$(CONFIG_OPENVPN_$(1)_ENABLE_NSS_DCA)
PKG_BUILD_DEPENDS:=$(foreach variant,$(OPENVPN_VARIANTS),OPENVPN_$(variant)_ENABLE_NSS_DCA:qca-nss-ovpn-12)
PKG_CONFIG_DEPENDS+=$(foreach variant,$(OPENVPN_VARIANTS),CONFIG_OPENVPN_$(variant)_ENABLE_NSS_DCA)
endif

include $(INCLUDE_DIR)/package.mk

define Package/openvpn/Default
  TITLE:=Open source VPN solution using $(2)
  SECTION:=net
  CATEGORY:=Network
  URL:=http://openvpn.net
  SUBMENU:=VPN
  MENU:=2
  DEPENDS:=+kmod-tun \
	   +libcap-ng \
	   +OPENVPN_$(1)_ENABLE_LZO:liblzo \
	   +OPENVPN_$(1)_ENABLE_LZ4:liblz4 \
	   +OPENVPN_$(1)_ENABLE_IPROUTE2:ip \
	   +OPENVPN_$(1)_ENABLE_DCO:libnl-genl \
	   +OPENVPN_$(1)_ENABLE_DCO:kmod-ovpn-backports \
	   $(3)
  EXTRA_DEPENDS:=$(if $(call OPENVPN_NSS_ENABLED,$(1)),kmod-qca-nss-ovpn-12)
  VARIANT:=$(1)
  PROVIDES:=openvpn openvpn-crypto
endef

Package/openvpn-openssl=$(call Package/openvpn/Default,openssl,OpenSSL,+PACKAGE_openvpn-openssl:libopenssl)
Package/openvpn-mbedtls=$(call Package/openvpn/Default,mbedtls,mbedTLS,+PACKAGE_openvpn-mbedtls:libmbedtls)
Package/openvpn-wolfssl=$(call Package/openvpn/Default,wolfssl,WolfSSL,+PACKAGE_openvpn-wolfssl:libwolfssl @BROKEN)

define Package/openvpn/config/Default
	source "$(SOURCE)/Config-$(1).in"
$(if $(and $(filter openssl,$(1)),$(wildcard $(TOPDIR)/$(OPENVPN_NSS_CONFIG))),	source "$(OPENVPN_NSS_CONFIG)")
endef

Package/openvpn-openssl/config=$(call Package/openvpn/config/Default,openssl)
Package/openvpn-mbedtls/config=$(call Package/openvpn/config/Default,mbedtls)
Package/openvpn-wolfssl/config=$(call Package/openvpn/config/Default,wolfssl)

ifeq ($(BUILD_VARIANT),mbedtls)
CONFIG_OPENVPN_MBEDTLS:=y
endif
ifeq ($(BUILD_VARIANT),openssl)
CONFIG_OPENVPN_OPENSSL:=y
endif
ifeq ($(BUILD_VARIANT),wolfssl)
CONFIG_OPENVPN_WOLFSSL:=y
endif

CONFIGURE_VARS += \
	IPROUTE=/sbin/ip \
	NETSTAT=/sbin/netstat

ifneq ($(call OPENVPN_NSS_ENABLED,$(BUILD_VARIANT)),)
TARGET_CPPFLAGS += -I$(STAGING_DIR)/usr/include/qca-nss-ovpn-12
endif

define Build/Configure
	$(call Build/Configure/Default, \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_SMALL),--enable-small) \
		--disable-selinux \
		--disable-systemd \
		--disable-plugins \
		--disable-debug \
		--disable-pkcs11 \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_LZO),--enable,--disable)-lzo \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_LZ4),--enable,--disable)-lz4 \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_X509_ALT_USERNAME),--enable,--disable)-x509-alt-username \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_MANAGEMENT),--enable,--disable)-management \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_FRAGMENT),--enable,--disable)-fragment \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_IPROUTE2),--enable,--disable)-iproute2 \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_PORT_SHARE),--enable,--disable)-port-share \
		$(if $(CONFIG_OPENVPN_$(BUILD_VARIANT)_ENABLE_DCO),--enable,--disable)-dco \
		$(if $(call OPENVPN_NSS_ENABLED,$(BUILD_VARIANT)),--enable,--disable)-nss-dca \
		$(if $(CONFIG_OPENVPN_OPENSSL),--with-crypto-library=openssl --with-openssl-engine=no) \
		$(if $(CONFIG_OPENVPN_MBEDTLS),--with-crypto-library=mbedtls) \
		$(if $(CONFIG_OPENVPN_WOLFSSL),--with-crypto-library=wolfssl) \
	)
endef

define Package/openvpn-$(BUILD_VARIANT)/conffiles
/etc/config/openvpn
/etc/openvpn.user
endef

define Package/openvpn-$(BUILD_VARIANT)/install
	$(INSTALL_DIR) \
		$(1)/usr/sbin \
		$(1)/usr/share/openvpn \
		$(1)/etc/init.d \
		$(1)/etc/config \
		$(1)/etc/openvpn \
		$(1)/lib/functions \
		$(1)/lib/upgrade/keep.d \
		$(1)/usr/libexec \
		$(1)/etc/hotplug.d/openvpn

	$(INSTALL_BIN) \
		$(PKG_INSTALL_DIR)/usr/sbin/openvpn \
		$(1)/usr/sbin/

	$(INSTALL_BIN) \
		files/openvpn.init \
		$(1)/etc/init.d/openvpn

	$(INSTALL_BIN) \
		files/usr/libexec/openvpn-hotplug \
		$(1)/usr/libexec/openvpn-hotplug

	$(INSTALL_DATA) \
		files/lib/functions/openvpn.sh \
		$(1)/lib/functions/openvpn.sh

	$(INSTALL_DATA) \
		files/etc/hotplug.d/openvpn/01-user \
		$(1)/etc/hotplug.d/openvpn/01-user

	$(INSTALL_DATA) \
		files/etc/openvpn.user \
		$(1)/etc/openvpn.user

	$(INSTALL_DATA) \
		files/openvpn.options \
		$(1)/usr/share/openvpn/openvpn.options

	$(INSTALL_CONF) files/openvpn.config \
		$(1)/etc/config/openvpn

	$(INSTALL_DATA) \
		files/openvpn.upgrade \
		$(1)/lib/upgrade/keep.d/openvpn
endef

$(eval $(call BuildPackage,openvpn-openssl))
$(eval $(call BuildPackage,openvpn-mbedtls))
$(eval $(call BuildPackage,openvpn-wolfssl))
