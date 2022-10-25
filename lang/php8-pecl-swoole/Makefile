#
# This is free software, licensed under the GNU General Public License v2.
# See /LICENSE for more information.
#
 
include $(TOPDIR)/rules.mk
 
PECL_NAME:=swoole
PECL_LONGNAME:=Bindings for the libswoole library
 
PKG_VERSION:=4.8.12
PKG_RELEASE:=1
PKG_HASH:=0a0b844992d26c974957749ada86a9ff7a88daa512b7255db4fc41afa062f4a4
 
PKG_NAME:=php8-pecl-$(PECL_NAME)
PKG_SOURCE:=$(PECL_NAME)-$(PKG_VERSION).tgz
PKG_SOURCE_URL:=http://pecl.php.net/get/
PKG_MAINTAINER:=Lean
PKG_LICENSE:=Apache2.0
PKG_LICENSE_FILES:=LICENSE
PKG_BUILD_DEPENDS:=php8
PKG_BUILD_DIR:=$(BUILD_DIR)/pecl-php8/$(PECL_NAME)-$(PKG_VERSION)
PKG_BUILD_PARALLEL:=1
 
include $(INCLUDE_DIR)/package.mk
include $(INCLUDE_DIR)/nls.mk
include $(TOPDIR)/feeds/packages/lang/php8/pecl.mk
 
 
# CONFIGURE_ARGS+= \
# 	--enable-openssl \
# 	--enable-http2 \
# 	--enable-sockets \
# 	--enable-debug \
# 	--enable-debug-log
 
$(eval $(call PHP8PECLPackage,$(PECL_NAME),$(PECL_LONGNAME),+libstdcpp,15))
$(eval $(call BuildPackage,$(PKG_NAME)))