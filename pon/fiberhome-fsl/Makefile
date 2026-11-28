include $(TOPDIR)/rules.mk
include $(INCLUDE_DIR)/kernel.mk

PKG_NAME:=fiberhome-fsl
PKG_RELEASE:=1
PKG_LICENSE:=GPL-2.0-only
PKG_URL:=https://github.com/LazuliKao/openwrt-fttr/tree/main/fiberhome-fsl
PKG_BUILD_DIR:=$(KERNEL_BUILD_DIR)/$(PKG_NAME)

include $(INCLUDE_DIR)/package.mk

define KernelPackage/fiberhome-fsl
  SUBMENU:=Other modules
  TITLE:=FiberHome FSL exclusive SPI transport
  DEPENDS:=@TARGET_airoha_an7581
  # Availability guard only (never "=y"): the module needs the SCU syscon
  # regmap; CONFIG_SPI is intentionally not forced (the module is bare MMIO,
  # not an SPI-framework driver). CONFIG_MFD_SYSCON is =y in airoha/an7581.
  KCONFIG:=CONFIG_MFD_SYSCON
  FILES:=$(PKG_BUILD_DIR)/fiberhome-fsl.ko
  AUTOLOAD:=$(call AutoProbe,fiberhome-fsl)
endef

define KernelPackage/fiberhome-fsl/description
  Bounded TX/RX SPI messages with exclusive open and device removal handling.
  FSL protocol and optical policy live in openwrt-fttr userspace. Requires an
  explicit fiberhome,an4475-fsl board node.
endef

# Copy exactly the three kbuild sources; Build/Prepare/Default would instead
# bulk-copy ./src/. wholesale, and the explicit list also promotes the kbuild
# files from ./src to the build dir top level.
define Build/Prepare
	$(CP) ./src/Makefile ./src/fsl.c ./src/fsl-uapi.h $(PKG_BUILD_DIR)/
endef

define Build/Compile
	$(KERNEL_MAKE) M="$(PKG_BUILD_DIR)" modules
endef

$(eval $(call KernelPackage,fiberhome-fsl))
