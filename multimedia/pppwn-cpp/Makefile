include $(TOPDIR)/rules.mk

PKG_NAME:=pppwn-cpp
PKG_RELEASE:=3

PKG_SOURCE_PROTO:=git
PKG_SOURCE_URL:=https://github.com/xfangfang/PPPwn_cpp.git
PKG_SOURCE_DATE:=2024-05-18
PKG_SOURCE_VERSION:=469101a775ae83d0cfb7475ffb2b42dcd9b7919f
PKG_MIRROR_HASH:=d7199002bc394b98b72573b776eacf0ef061a27d4cf96f5a147a9d47c2c3d91a

PKG_LICENSE:=GPL-3.0-only
PKG_LICENSE_FILES:=LICENSE

include $(INCLUDE_DIR)/package.mk
include $(INCLUDE_DIR)/cmake.mk

define Package/pppwn-cpp
  SECTION:=net
  CATEGORY:=Network
  TITLE:=C++ rewrite of PPPwn (PlayStation 4 PPPoE RCE)
  URL:=https://github.com/xfangfang/PPPwn_cpp
  DEPENDS:=+libpcap +libstdcpp
endef

define Package/pppwn-cpp/description
  PPPwn is a kernel remote code execution exploit for PlayStation 4 up to
  FW 11.00. This is a proof-of-concept exploit for CVE-2006-4304 that was
  reported responsibly to PlayStation.
endef

CMAKE_OPTIONS += -DUSE_SYSTEM_PCAPPLUSPLUS=OFF

define Build/Install
endef

define Package/pppwn-cpp/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_DIR) $(1)/etc/pppwn
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/pppwn $(1)/usr/bin/pppwn
	$(INSTALL_DATA) ./files/* $(1)/etc/pppwn/
endef

$(eval $(call BuildPackage,pppwn-cpp))
