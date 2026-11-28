include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-fttr
PKG_VERSION:=2026.11.27.165847
PKG_RELEASE:=1
PKG_LICENSE:=Apache-2.0
PKG_PO_VERSION:=$(PKG_VERSION)-r$(PKG_RELEASE)

LUCI_TITLE:=FTTR management interface
LUCI_DEPENDS:=+luci-base +rpcd +openwrt-fttr
LUCI_DESCRIPTION:=LuCI overview for the FTTR controller and downstream ONUs.
LUCI_MAINTAINER:=LazuliKao
# Rsbuild already minifies modern JS that the legacy jsmin cannot parse.
LUCI_MINIFY_JS:=0

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
