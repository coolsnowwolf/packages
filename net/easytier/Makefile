include $(TOPDIR)/rules.mk

PKG_NAME:=easytier
PKG_VERSION:=2.6.4
PKG_RELEASE:=1

PKG_SOURCE:=$(PKG_NAME)-$(PKG_VERSION).tar.gz
PKG_SOURCE_URL:=https://codeload.github.com/EasyTier/EasyTier/tar.gz/v$(PKG_VERSION)?
PKG_HASH:=352c0866da709415a837405a6ce4f51b8dfae27e5d5c1da1fb4d8f7338e46795
PKG_BUILD_DIR:=$(BUILD_DIR)/EasyTier-$(PKG_VERSION)

PKG_LICENSE:=LGPL-3.0-only
PKG_LICENSE_FILES:=LICENSE

PKG_BUILD_DEPENDS:=rust/host protobuf/host
PKG_BUILD_PARALLEL:=1
PKG_USE_MIPS16:=0

include $(INCLUDE_DIR)/package.mk
include ../../lang/rust/rust-package.mk

define Package/easytier
	SECTION:=net
	CATEGORY:=Network
	TITLE:=A simple, decentralized mesh VPN with WireGuard support.
	DEPENDS:=$(RUST_ARCH_DEPENDS) @!(i386) +kmod-tun +libatomic
	URL:=https://github.com/EasyTier/EasyTier
endef

define Package/easytier/description
  A simple, decentralized mesh VPN with WireGuard support.
endef

define Package/easytier/config
  if PACKAGE_easytier
    config EASYTIER_WEB
	bool "Build Web Console (easytier-web)"
	depends on !(mips || mipsel)
	default y
  endif
endef

WEB_FILE:=$(PKG_NAME)-web-v$(PKG_VERSION).zip
define Download/easytier-web
	URL:=https://github.com/EasyTier/EasyTier/releases/download/v$(PKG_VERSION)/
	URL_FILE:=easytier-web-dashboard-v$(PKG_VERSION).zip
	FILE:=$(WEB_FILE)
	HASH:=032a1d19f956c204497869ca956bde254c823785ebaf22bc2d88b8439b2a5030
endef

define Build/Prepare
	$(call Build/Prepare/Default)
ifeq ($(CONFIG_EASYTIER_WEB),y)
	$(eval $(call Download,easytier-web))
	( \
		mkdir -p $(PKG_BUILD_DIR)/easytier-web/frontend/dist ; \
		unzip -q -j -d $(PKG_BUILD_DIR)/easytier-web/frontend/dist $(DL_DIR)/$(WEB_FILE) ; \
	)
endif
endef

CARGO_PKG_VARS += PROTOC=$(STAGING_DIR_HOSTPKG)/bin/protoc
CARGO_RUSTFLAGS += -C link-args=-latomic

define Build/Compile
	$(call Build/Compile/Cargo,easytier)
ifeq ($(CONFIG_EASYTIER_WEB),y)
	$(call Build/Compile/Cargo,easytier-web,--features=embed)
endif
endef

define Package/easytier/install
	$(INSTALL_DIR) $(1)/usr/bin/
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/* $(1)/usr/bin/
endef

$(eval $(call RustBinPackage,easytier))
$(eval $(call BuildPackage,easytier))
