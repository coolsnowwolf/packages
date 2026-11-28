include $(TOPDIR)/rules.mk

PKG_NAME:=h3c-fmcs-firmware
PKG_VERSION:=1.0
PKG_RELEASE:=1
PKG_LICENSE:=Proprietary

PKG_FLAGS:=nonshared

include $(INCLUDE_DIR)/package.mk

define Package/h3c-fmcs-firmware
  SECTION:=firmware
  CATEGORY:=Firmware
  TITLE:=H3C HM2004-DU Micro-OLT FPGA Bitstream
  DEPENDS:=@TARGET_airoha
endef

define Package/h3c-fmcs-firmware/description
  FPGA bitstream (FTTR_TOP.sbit) for H3C HM2004-DU downstream Micro-OLT controller.
endef

define Build/Compile
endef

define Package/h3c-fmcs-firmware/install
	$(INSTALL_DIR) $(1)/lib/firmware
	$(INSTALL_DATA) ./files/FTTR_TOP.sbit $(1)/lib/firmware/FTTR_TOP.sbit
endef

$(eval $(call BuildPackage,h3c-fmcs-firmware))
