# SPDX-License-Identifier: GPL-2.0-only
#
# Copyright (C) 2023 Luca Barbato and Donald Hoskins

# Clear environment variables which should be handled internally,
# as users might configure their own env on the host

# CCache
unexport RUSTC_WRAPPER

comma ?= ,

define rust_arch_to_llvm_target
$(strip \
  $(if $(filter x86_64 amd64 i386 i486 i586 i686 x86,$(strip $(1))),X86) \
  $(if $(filter aarch64 aarch64_be arm64,$(strip $(1))),AArch64) \
  $(if $(filter arm armeb armv%,$(strip $(1))),ARM) \
  $(if $(filter mips mipsel mips64 mips64el,$(strip $(1))),Mips) \
  $(if $(filter powerpc powerpc64 powerpc64le ppc%,$(strip $(1))),PowerPC) \
  $(if $(filter riscv32 riscv64 riscv%,$(strip $(1))),RISCV) \
  $(if $(filter loongarch32 loongarch64 loongarch%,$(strip $(1))),LoongArch) \
  $(if $(filter sparc sparc64 sparcv9,$(strip $(1))),Sparc) \
  $(if $(filter s390 s390x,$(strip $(1))),SystemZ) \
  $(if $(filter arc arceb archs%,$(strip $(1))),ARC) \
  $(if $(filter csky,$(strip $(1))),CSKY) \
  $(if $(filter m68k,$(strip $(1))),M68k) \
  $(if $(filter xtensa,$(strip $(1))),Xtensa) \
  $(if $(filter avr avr32,$(strip $(1))),AVR) \
  $(if $(filter msp430,$(strip $(1))),MSP430) \
  $(if $(filter hexagon,$(strip $(1))),Hexagon) \
  $(if $(filter lanai,$(strip $(1))),Lanai) \
  $(if $(filter ve,$(strip $(1))),VE) \
  $(if $(filter xcore,$(strip $(1))),XCore) \
  $(if $(filter bpf bpfeb bpfel,$(strip $(1))),BPF) \
  $(if $(filter wasm32 wasm64,$(strip $(1))),WebAssembly))
endef

RUST_LLVM_HOST_TARGET:=$(call rust_arch_to_llvm_target,$(HOST_ARCH))
RUST_LLVM_CROSS_TARGET:=$(call rust_arch_to_llvm_target,$(ARCH))
RUST_LLVM_EXTRA_TARGETS:=$(strip $(subst $(comma), ,$(call qstrip,$(CONFIG_RUST_LLVM_EXTRA_TARGETS))))
RUST_LLVM_NEEDED_TARGETS:=$(strip \
	$(RUST_LLVM_HOST_TARGET) \
	$(filter-out $(RUST_LLVM_HOST_TARGET),$(RUST_LLVM_CROSS_TARGET)) \
	$(filter-out $(RUST_LLVM_HOST_TARGET) $(RUST_LLVM_CROSS_TARGET),$(RUST_LLVM_EXTRA_TARGETS)))

# Rust Environmental Vars
RUSTC_HOST_SUFFIX:=$(word 4, $(subst -, ,$(GNU_HOST_NAME)))
RUSTC_HOST_ARCH:=$(HOST_ARCH)-unknown-linux-$(RUSTC_HOST_SUFFIX)
CARGO_HOME:=$(DL_DIR)/cargo

ifeq ($(CONFIG_USE_MUSL),y)
  # Force linking of the SSP library for musl
  ifdef CONFIG_PKG_CC_STACKPROTECTOR_REGULAR
    ifeq ($(strip $(PKG_SSP)),1)
      RUSTC_LDFLAGS+=-lssp_nonshared
    endif
  endif
  ifdef CONFIG_PKG_CC_STACKPROTECTOR_STRONG
    ifeq ($(strip $(PKG_SSP)),1)
      RUSTC_LDFLAGS+=-lssp_nonshared
    endif
  endif
endif

CARGO_RUSTFLAGS+=-Ctarget-feature=-crt-static $(RUSTC_LDFLAGS)

ifeq ($(HOST_OS),Darwin)
  ifeq ($(HOST_ARCH),arm64)
    RUSTC_HOST_ARCH:=aarch64-apple-darwin
  else ifeq ($(HOST_ARCH),x86_64)
    RUSTC_HOST_ARCH:=x86_64-apple-darwin
  endif
endif

# mips64 openwrt has a specific target in rustc
ifeq ($(ARCH),mips64)
  RUSTC_TARGET_ARCH:=$(REAL_GNU_TARGET_NAME)
else
  RUSTC_TARGET_ARCH:=$(subst openwrt,unknown,$(REAL_GNU_TARGET_NAME))
endif

RUSTC_TARGET_ARCH:=$(subst muslgnueabi,musleabi,$(RUSTC_TARGET_ARCH))

ifeq ($(ARCH),i386)
  RUSTC_TARGET_ARCH:=$(subst i486,i586,$(RUSTC_TARGET_ARCH))
else ifeq ($(ARCH),riscv64)
  RUSTC_TARGET_ARCH:=$(subst riscv64,riscv64gc,$(RUSTC_TARGET_ARCH))
endif

# 32-bit PowerPC has both soft-float and hard-float subtargets: mpc85xx is an
# e500v2 with no floating-point unit at all, while apm821xx (powerpc_464fp)
# has one. They share the powerpc-unknown-linux-musl triple, which is the
# hard-float one, so on the soft-float subtarget rustc emits lfd, stfd and
# fdiv for a core with no floating-point registers, and passes doubles in
# registers nothing else in the image uses.
#
# CONFIG_SOFT_FLOAT already carries this: it defaults to y when the target has
# no fpu feature, and it is what puts --with-float=soft into the toolchain and
# -msoft-float into TARGET_CFLAGS.
ifeq ($(ARCH),powerpc)
  ifeq ($(CONFIG_SOFT_FLOAT),y)
    RUSTC_TARGET_RUSTFLAGS+=-Ctarget-feature=-hard-float
  endif
endif

CARGO_RUSTFLAGS+=$(RUSTC_TARGET_RUSTFLAGS)

# ARM Logic
ifeq ($(ARCH),arm)
  ifeq ($(CONFIG_arm_v6)$(CONFIG_arm_v7),)
    RUSTC_TARGET_ARCH:=$(subst arm,armv5te,$(RUSTC_TARGET_ARCH))
  else ifeq ($(CONFIG_arm_v7),y)
    RUSTC_TARGET_ARCH:=$(subst arm,armv7,$(RUSTC_TARGET_ARCH))
  endif

  ifeq ($(CONFIG_HAS_FPU),y)
    RUSTC_TARGET_ARCH:=$(subst musleabi,musleabihf,$(RUSTC_TARGET_ARCH))
    RUSTC_TARGET_ARCH:=$(subst gnueabi,gnueabihf,$(RUSTC_TARGET_ARCH))
  endif
endif

ifeq ($(ARCH),aarch64)
    RUSTC_CFLAGS:=-mno-outline-atomics
endif

# The same flags have to reach the std that rust/host builds for the target,
# or std and the packages linked against it disagree about how a double is
# passed. Bootstrap sets RUSTFLAGS itself on the cargo invocations it spawns,
# so setting that would be overwritten; it does however read
# CARGO_TARGET_<TRIPLE>_RUSTFLAGS and fold it in, scoped to that triple, which
# leaves the host artifacts built in the same run untouched.
# Deferred, because the blocks above still rewrite RUSTC_TARGET_ARCH.
RUSTC_TARGET_UPPER = $(subst -,_,$(call toupper,$(RUSTC_TARGET_ARCH)))
RUSTC_TARGET_CARGO_RUSTFLAGS = CARGO_TARGET_$(RUSTC_TARGET_UPPER)_RUSTFLAGS

# Support only a subset for now.
RUST_ARCH_DEPENDS:=@(aarch64||arm||i386||loongarch64||mips||mips64||mips64el||mipsel||powerpc||powerpc64||riscv64||x86_64)

ifneq ($(CONFIG_RUST_SCCACHE),)
  RUST_SCCACHE_DIR:=$(if $(call qstrip,$(CONFIG_RUST_SCCACHE_DIR)),$(call qstrip,$(CONFIG_RUST_SCCACHE_DIR)),$(TOPDIR)/.sccache)

  RUST_SCCACHE_VARS:= \
	CARGO_INCREMENTAL=0 \
	RUSTC_WRAPPER=sccache \
	SCCACHE_DIR=$(RUST_SCCACHE_DIR)
endif

CARGO_HOST_CONFIG_VARS= \
	$(RUST_SCCACHE_VARS) \
	CARGO_HOME=$(CARGO_HOME)

CARGO_HOST_PROFILE:=release

CARGO_PKG_CONFIG_VARS= \
	$(RUST_SCCACHE_VARS) \
	CARGO_BUILD_TARGET=$(RUSTC_TARGET_ARCH) \
	CARGO_HOME=$(CARGO_HOME) \
	CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
	CARGO_PROFILE_RELEASE_DEBUG=false \
	CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=false \
	CARGO_PROFILE_RELEASE_LTO=true \
	CARGO_PROFILE_RELEASE_OPT_LEVEL=z \
	CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true \
	CARGO_PROFILE_RELEASE_PANIC=unwind \
	CARGO_PROFILE_RELEASE_RPATH=false \
	CARGO_TARGET_$(RUSTC_TARGET_UPPER)_LINKER=$(TARGET_CC_NOCACHE) \
	RUSTFLAGS="$(CARGO_RUSTFLAGS)" \
	TARGET_CC=$(TARGET_CC_NOCACHE) \
	TARGET_CFLAGS="$(TARGET_CFLAGS) $(RUSTC_CFLAGS)"

CARGO_PKG_PROFILE:=$(if $(CONFIG_DEBUG),dev,release)

CARGO_RUSTFLAGS+=-Clink-arg=-fuse-ld=$(TARGET_LINKER)
