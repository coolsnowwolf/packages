include $(TOPDIR)/rules.mk

PKG_NAME:=easytier-next
PKG_VERSION:=2.6.4
PKG_RELEASE:=1

# Architecture mapping for pre-built binaries
ifeq ($(ARCH),mipsel)
	APP_ARCH:=mipsel
endif
ifeq ($(ARCH),mips)
	APP_ARCH:=mips
endif
ifeq ($(ARCH),arm)
	APP_ARCH:=arm
endif
ifeq ($(BOARD),kirkwood)
	APP_ARCH:=arm
endif
ifeq ($(ARCH),armv7)
	APP_ARCH:=armv7
endif
ifeq ($(ARCH),aarch64)
	APP_ARCH:=aarch64
endif
ifeq ($(ARCH),arm64)
	APP_ARCH:=aarch64
endif
ifeq ($(ARCH),armv8)
	APP_ARCH:=aarch64
endif
ifeq ($(ARCH),x86_64)
	APP_ARCH:=x86_64
endif
ifeq ($(ARCH),riscv64)
	APP_ARCH:=riscv64
endif
ifeq ($(ARCH),loongarch64)
	APP_ARCH:=loongarch64
endif

PKG_SOURCE:=easytier-linux-$(APP_ARCH)-v$(PKG_VERSION).zip
PKG_SOURCE_URL:=https://github.com/EasyTier/EasyTier/releases/download/v$(PKG_VERSION)/
PKG_HASH:=skip

PKG_LICENSE:=Apache-2.0
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=Dante <dante@seckv.com>

PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_VERSION)

include $(INCLUDE_DIR)/package.mk

define Package/$(PKG_NAME)
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=VPN
  TITLE:=A simple, decentralized mesh VPN with WireGuard support (Next)
  URL:=https://github.com/EasyTier/EasyTier
  DEPENDS:=@(x86_64||arm||aarch64||mipsel||mips||riscv64||loongarch64) +kmod-tun
  PROVIDES:=easytier
  CONFLICTS:=easytier
endef

define Package/$(PKG_NAME)/description
  A simple, decentralized mesh VPN with WireGuard support.
endef

define Package/$(PKG_NAME)/config
	config EASYTIER_INCLUDE_WEBCONSOLE
		bool "Include Web Console (easytier-web)"
		depends on PACKAGE_$(PKG_NAME)
		depends on !(mips || mipsel)
		default y
		help
		  Install the easytier-web web console.
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	unzip -q -o $(DL_DIR)/$(PKG_SOURCE) -d $(PKG_BUILD_DIR)
	find $(PKG_BUILD_DIR) -mindepth 2 -type f -exec mv -f {} $(PKG_BUILD_DIR)/ \;
endef

define Build/Compile
endef

define Package/$(PKG_NAME)/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/easytier-core $(1)/usr/bin/
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/easytier-cli $(1)/usr/bin/
ifdef CONFIG_EASYTIER_INCLUDE_WEBCONSOLE
	if [ -f $(PKG_BUILD_DIR)/easytier-web-embed ]; then \
		$(INSTALL_BIN) $(PKG_BUILD_DIR)/easytier-web-embed $(1)/usr/bin/easytier-web; \
	elif [ -f $(PKG_BUILD_DIR)/easytier-web ]; then \
		$(INSTALL_BIN) $(PKG_BUILD_DIR)/easytier-web $(1)/usr/bin/easytier-web; \
	fi
endif
endef

$(eval $(call BuildPackage,$(PKG_NAME)))
