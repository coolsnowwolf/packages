#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#
 
include $(TOPDIR)/rules.mk
 
PECL_NAME:=yaf
PECL_LONGNAME:=Yaf is a PHP framework similar to zend framework
 
PKG_VERSION:=3.3.5
PKG_RELEASE:=1
PKG_HASH:=cbbe7f2abbd2bb93074c6749c40fe4b660846361c0d9e258eee2b4005f8ac620
 
PKG_NAME:=php7-pecl-$(PECL_NAME)
PKG_SOURCE:=$(PECL_NAME)-$(PKG_VERSION).tgz
PKG_SOURCE_URL:=http://pecl.php.net/get/
PKG_MAINTAINER:=Lean
PKG_LICENSE:=Apache2.0
PKG_LICENSE_FILES:=LICENSE
PKG_BUILD_DEPENDS:=php7
PKG_BUILD_DIR:=$(BUILD_DIR)/pecl-php7/$(PECL_NAME)-$(PKG_VERSION)
PKG_BUILD_PARALLEL:=1
 
include $(INCLUDE_DIR)/package.mk
include $(INCLUDE_DIR)/nls.mk
include $(TOPDIR)/feeds/packages/lang/php7/pecl.mk
 
$(eval $(call PHP7PECLPackage,$(PECL_NAME),$(PECL_LONGNAME),+libstdcpp,15))
$(eval $(call BuildPackage,$(PKG_NAME)))