# Kernel package for H3C HM2004-DU Downlink Micro-OLT FPGA Controller (FMCS)

include $(TOPDIR)/rules.mk
include $(INCLUDE_DIR)/kernel.mk

PKG_NAME:=h3c-fmcs
PKG_RELEASE:=1
PKG_LICENSE:=GPL-2.0-only
PKG_URL:=https://github.com/LazuliKao/openwrt-fttr/tree/main/h3c-fmcs

# The backport detects the driver and flowtable APIs at build time.
PATCH_DIR:=./patches-5.4

include $(INCLUDE_DIR)/package.mk

define KernelPackage/h3c-fmcs
  SUBMENU:=Network Devices
  TITLE:=H3C HM2004-DU Micro-OLT FPGA Management Controller (FMCS)
  DEPENDS:=@(TARGET_airoha_an7581) +h3c-fmcs-firmware +kmod-spi-gpio +kmod-nf-flow
  FILES:=$(PKG_BUILD_DIR)/h3c-fmcs.ko
  AUTOLOAD:=$(call AutoProbe,h3c-fmcs)
  KCONFIG:=
endef

define KernelPackage/h3c-fmcs/description
  Linux 6.x native driver for H3C HM2004-DU downlink Micro-OLT FPGA (FMCS).
  Manages SPI0 register transport, IRQ 26 hardware event notifications,
  /dev/fmcs_mci character control interface, and molt_omci/molt_ploam virtual netdevs.
endef

define Build/Compile
	$(KERNEL_MAKE) M="$(PKG_BUILD_DIR)" modules
endef

$(eval $(call KernelPackage,h3c-fmcs))
