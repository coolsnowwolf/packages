#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#

include $(TOPDIR)/rules.mk

PKG_NAME:=gowebdav
PKG_VERSION:=4.2.0
PKG_RELEASE:=$(AUTORELEASE)

PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://codeload.github.com/hacdias/webdav/tar.gz/v$(PKG_VERSION)?
PKG_HASH:=bc8e11cf765e5506677968ccdf815e0648d93606dc3c5e8f5f2f76fe0407ee2c

PKG_LICENSE:=MIT
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=ElonH <elonhhuang@gmail.com>

PKG_BUILD_DIR:=$(BUILD_DIR)/webdav-$(PKG_VERSION)
PKG_BUILD_DEPENDS:=golang/host
PKG_BUILD_PARALLEL:=1
PKG_USE_MIPS16:=0

GO_PKG:=github.com/hacdias/webdav
GO_PKG_LDFLAGS:=-s -w

include $(INCLUDE_DIR)/package.mk
include ../../lang/golang/golang-package.mk

define Package/gowebdav
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=File Transfer
  TITLE:=Simple Go WebDAV server
  URL:=https://github.com/hacdias/webdav
  DEPENDS:=$(GO_ARCH_DEPENDS)
endef

define Package/gowebdav/description
Webdav command line interface is really easy to use so you can easily create
a WebDAV server for your own user. By default, it runs on a random free port
and supports JSON, YAML and TOML configuration.
endef

define Package/gowebdav/install
	$(call GoPackage/Package/Install/Bin,$(1))

	$(INSTALL_DIR) $(1)/etc/config/ $(1)/etc/init.d/ $(1)/etc/webdav/
	$(INSTALL_BIN) $(CURDIR)/files/webdav.init $(1)/etc/init.d/webdav
	$(INSTALL_CONF) $(CURDIR)/files/webdav.config $(1)/etc/config/webdav
	$(INSTALL_CONF) $(CURDIR)/files/webdav.yaml $(1)/etc/webdav/webdav.yaml
endef

$(eval $(call GoBinPackage,gowebdav))
$(eval $(call BuildPackage,gowebdav))
