# SPDX-License-Identifier: GPL-2.0-only
#
# Copyright (C) 2023 Luca Barbato and Donald Hoskins

include $(TOPDIR)/rules.mk

PKG_NAME:=rust
PKG_VERSION:=1.99.1
PKG_RELEASE:=2

PKG_SOURCE:=rustc-$(PKG_VERSION)-src.tar.xz
PKG_SOURCE_URL:=https://static.rust-lang.org/dist/
PKG_HASH:=6451598a99e77f07571960f6b21d577399d3a30f33e80cbd2e3a4aae6535d265
HOST_BUILD_DIR:=$(BUILD_DIR)/host/rustc-$(PKG_VERSION)-src

PKG_MAINTAINER:=Luca Barbato <lu_zero@luminem.org>
PKG_LICENSE:=Apache-2.0 MIT
PKG_LICENSE_FILES:=LICENSE-APACHE LICENSE-MIT
PKG_CPE_ID:=cpe:/a:rust-lang:rust

PKG_HOST_ONLY:=1
PKG_USE_MIPS16:=0

include $(INCLUDE_DIR)/host-build.mk
include $(INCLUDE_DIR)/package.mk
include ./rust-values.mk

RUST_PKG_DIR:=$(patsubst %/,%,$(dir $(lastword $(MAKEFILE_LIST))))

define Package/rust
  SECTION:=lang
  CATEGORY:=Languages
  SUBMENU:=Rust
  TITLE:=Rust Programming Language Compiler
  URL:=https://www.rust-lang.org/
  DEPENDS:=$(RUST_ARCH_DEPENDS)
endef

define Package/rust/description
  Rust is a multi-paradigm, general-purpose programming language designed for performance
  and safety, especially safe concurrency. Rust is syntactically similar to C++, but can
  guarantee memory safety by using a borrow checker to validate references.
endef

define Package/rust/config
  source "$(SOURCE)/Config.in"
endef

# Rust-lang has an uninstall script
RUST_UNINSTALL:=$(STAGING_DIR)/host/lib/rustlib/uninstall.sh

# Target Flags
TARGET_CONFIGURE_ARGS = \
	--set=target.$(RUSTC_TARGET_ARCH).ar=$(TARGET_AR) \
	--set=target.$(RUSTC_TARGET_ARCH).cc=$(TARGET_CC_NOCACHE) \
	--set=target.$(RUSTC_TARGET_ARCH).cxx=$(TARGET_CXX_NOCACHE) \
	--set=target.$(RUSTC_TARGET_ARCH).linker=$(TARGET_CC_NOCACHE) \
	--set=target.$(RUSTC_TARGET_ARCH).ranlib=$(TARGET_RANLIB) \
	--set=target.$(RUSTC_TARGET_ARCH).crt-static=false \
	$(if $(CONFIG_USE_MUSL),--set=target.$(RUSTC_TARGET_ARCH).musl-root=$(TOOLCHAIN_ROOT_DIR))

# CARGO_HOME is an environmental
HOST_CONFIGURE_VARS += CARGO_HOME="$(CARGO_HOME)"

# Rust Configuration Arguments
HOST_CONFIGURE_ARGS = \
	--build=$(RUSTC_HOST_ARCH) \
	--target=$(RUSTC_TARGET_ARCH),$(RUSTC_HOST_ARCH) \
	--host=$(RUSTC_HOST_ARCH) \
	--prefix=$(STAGING_DIR)/host \
	--bindir=$(STAGING_DIR)/host/bin \
	--libdir=$(STAGING_DIR)/host/lib \
	--sysconfdir=$(STAGING_DIR)/host/etc \
	--datadir=$(STAGING_DIR)/host/share \
	--mandir=$(STAGING_DIR)/host/man \
	--dist-compression-formats=gz \
	--disable-sanitizers \
	--release-channel=stable \
	--enable-cargo-native-static \
	--bootstrap-cache-path=$(DL_DIR)/rustc \
	$(TARGET_CONFIGURE_ARGS)

define Host/Configure
	( \
		set -e; \
		ARGS_FILE="$(HOST_BUILD_DIR)/.host-llvm-configure-args"; \
		$(if $(RUST_HOST_LLVM_CONFIG),RUST_HOST_LLVM_CONFIG="$(RUST_HOST_LLVM_CONFIG)") \
		$(if $(RUST_LLVM_SYS_ROOT),RUST_LLVM_SYS_ROOT="$(RUST_LLVM_SYS_ROOT)") \
		sh "$(RUST_PKG_DIR)/select-llvm.sh" \
			--src-dir "$(HOST_BUILD_DIR)" \
			--build-dir "$(HOST_BUILD_DIR)" \
			--host-triple "$(RUSTC_HOST_ARCH)" \
			--needed-targets "$(RUST_LLVM_NEEDED_TARGETS)" \
			--use-host-llvm "$(if $(filter y,$(CONFIG_RUST_USE_HOST_LLVM)),y,n)" \
			--download-ci-llvm "$(if $(filter y,$(CONFIG_RUST_DOWNLOAD_CI_LLVM)),y,n)" \
			--hostcxx "$(if $(HOSTCXX),$(HOSTCXX),c++)" \
			--out-args "$$$$ARGS_FILE"; \
		set -- $(HOST_CONFIGURE_ARGS); \
		while IFS= read -r arg || [ -n "$$$$arg" ]; do \
			[ -n "$$$$arg" ] && set -- "$$$$@" "$$$$arg"; \
		done < "$$$$ARGS_FILE"; \
		cd "$(HOST_BUILD_DIR)" && $(HOST_CONFIGURE_VARS) ./configure "$$$$@" ; \
	)
endef

define Host/Uninstall
	# Call the Uninstall script
	[ -f $(RUST_UNINSTALL) ] && \
		$(BASH) $(RUST_UNINSTALL) || echo No Uninstall
endef

define Host/Patch
	$(if $(HOST_QUILT),rm -rf $(HOST_BUILD_DIR)/patches; mkdir -p $(HOST_BUILD_DIR)/patches)
	$(if $(HOST_QUILT),$(call PatchDir/Quilt,$(HOST_BUILD_DIR),$(HOST_PATCH_DIR),))
	$(if $(HOST_QUILT),touch $(HOST_BUILD_DIR)/.quilt_used)
	$(if $(HOST_QUILT),,$(if $(wildcard $(HOST_PATCH_DIR)/*.patch), \
		$(foreach p,$(sort $(wildcard $(HOST_PATCH_DIR)/*.patch)), \
			echo "Applying patch $(notdir $p)" ; \
			$(PATCH) -f -p1 -d $(HOST_BUILD_DIR) < $p || \
			{ echo "Patch failed! Please fix: $(notdir $p)!" ; exit 1 ; } ; \
		) \
	))
endef

define Host/Compile
	( \
		HOST_LLVM_RUSTFLAGS="$$$$(cat $(HOST_BUILD_DIR)/.host-llvm-rustflags 2>/dev/null || true)"; \
		RUSTFLAGS_BOOTSTRAP_VAL="$(if $(RUSTFLAGS_BOOTSTRAP),$(strip $(RUSTFLAGS_BOOTSTRAP)),$$$${RUSTFLAGS_BOOTSTRAP:-})"; \
		if [ -n "$$$$HOST_LLVM_RUSTFLAGS" ]; then \
			RUSTFLAGS_BOOTSTRAP_VAL="$$$${RUSTFLAGS_BOOTSTRAP_VAL:+$$$$RUSTFLAGS_BOOTSTRAP_VAL }$$$$HOST_LLVM_RUSTFLAGS"; \
		fi; \
		if [ -n "$$$$RUSTFLAGS_BOOTSTRAP_VAL" ]; then \
			export RUSTFLAGS_BOOTSTRAP="$$$$RUSTFLAGS_BOOTSTRAP_VAL"; \
		fi; \
		$(RUST_SCCACHE_VARS) \
		CARGO_HOME=$(CARGO_HOME) \
		TARGET_CFLAGS="$(TARGET_CFLAGS)" \
		$(if $(RUSTC_TARGET_RUSTFLAGS),$(RUSTC_TARGET_CARGO_RUSTFLAGS)="$(RUSTC_TARGET_RUSTFLAGS)") \
		$(PYTHON) $(HOST_BUILD_DIR)/x.py \
			--build-dir $(HOST_BUILD_DIR)/build \
			--ci false \
			dist build-manifest rustc rust-std cargo llvm-tools rust-src ; \
	)
endef

define Host/Install
	( \
		cd $(HOST_BUILD_DIR)/build/dist ; \
		for targz in *.tar.gz; do \
			$(STAGING_DIR_HOST)/bin/libdeflate-gzip -dc "$$$$targz" | tar -xf - ; \
		done ; \
		find . -mindepth 2 -maxdepth 2 -type f -name install.sh \
			-execdir bash '{}' --prefix=$(STAGING_DIR)/host --disable-ldconfig \; ; \
	)
endef

$(eval $(call HostBuild))
$(eval $(call BuildPackage,rust))
