
include $(TOPDIR)/rules.mk

PKG_NAME:=webdav-hacdias
PKG_VERSION:=4.2.0
PKG_RELEASE:=$(AUTORELEASE)

PKG_SOURCE_PROTO:=git
PKG_SOURCE_URL:=https://github.com/hacdias/webdav.git
PKG_SOURCE_DATE:=2022-5-30
PKG_SOURCE_VERSION:=436a3b0
PKG_SOURCE_SUBDIR:=$(PKG_NAME)-$(PKG_VERSION)
PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION)-$(PKG_SOURCE_VERSION).tar.gz
PKG_MIRROR_HASH:=5587cf5bddfb5f8b57f8ad23e521ddfd6c623ebb2f7381555c3f20d7f147d7e8

PKG_LICENSE:=GPLv3
PKG_MAINTAINER:=ElonH <elonhhuang@gmail.com>

PKG_BUILD_DEPENDS:=golang/host
PKG_BUILD_PARALLEL:=1
PKG_USE_MIPS16:=0 # https://github.com/openwrt/packages/issues/8498
GO_PKG:=github.com/hacdias/webdav
GO_PKG_LDFLAGS:=-s -w

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/golang/golang-package.mk

define Package/$(PKG_NAME)
	SECTION:=net
        CATEGORY:=Network
        SUBMENU:=File Transfer
	TITLE:=A powerful WebDav server written in Golang.
	URL:=https://github.com/hacdias/webdav
	DEPENDS:=$(GO_ARCH_DEPENDS) 
endef

define Package/$(PKG_NAME)/description
webdav command line interface is really easy to use so you can easily create a WebDAV server for your own user.
By default, it runs on a random free port and supports JSON, YAML and TOML configuration.
endef

define Package/$(PKG_NAME)/install
	$(INSTALL_DIR) $(1)/usr/bin/ $(1)/etc/config/ $(1)/etc/init.d/ $(1)/etc/webdav/
	$(INSTALL_BIN) $(GO_PKG_BUILD_BIN_DIR)/webdav $(1)/usr/bin/
	$(INSTALL_CONF) $(CURDIR)/files/webdav.yaml $(1)/etc/webdav/webdav.yaml
	$(INSTALL_CONF) $(CURDIR)/files/webdav.config $(1)/etc/config/webdav
	$(INSTALL_BIN) $(CURDIR)/files/webdav.init $(1)/etc/init.d/webdav
endef

$(eval $(call GoBinPackage,$(PKG_NAME)))
$(eval $(call BuildPackage,$(PKG_NAME)))
