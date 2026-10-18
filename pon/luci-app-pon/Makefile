# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk

LUCI_TITLE:=LuCI PON configuration
LUCI_URL:=https://github.com/pbs05/openwrt-pon-userspace/tree/main/luci-app-pon
LUCI_DEPENDS:=+luci-base +airoha-ponctl +airoha-pond \
	+mtd +ubi-utils +nand-utils +jshn +jsonfilter
PKG_VERSION:=0.1.0
PKG_RELEASE:=5

PKG_LICENSE:=Apache-2.0 GPL-2.0-only

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage: feed scanner marker; luci.mk generates the package rules.
