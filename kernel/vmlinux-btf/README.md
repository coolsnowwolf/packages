# vmlinux-btf

OpenWrt package that generates a detached BTF (`vmlinux`) file from kernel headers and `.config`, enabling CO-RE (Compile Once – Run Everywhere) eBPF programs on OpenWrt without rebuilding or replacing the running kernel.

## Overview

Official OpenWrt release kernels typically omit `CONFIG_DEBUG_INFO_BTF=y`. Without kernel BTF, modern eBPF tools that rely on CO-RE (such as `libbpf`-based loaders) cannot resolve kernel type layouts.

Rather than recompiling the entire kernel tree, this package compiles lightweight stub sources (`src/shadow_btf_*.c`) against the prepared target kernel headers (`$(LINUX_DIR)`) using the target configuration and toolchain. `pahole` then extracts the detached BTF blob from the linked object.

## Requirements

- OpenWrt buildroot or SDK environment.
- Host `pahole` >= 1.22 (from the `dwarves` package).

## Building

### In OpenWrt SDK

```bash
git clone https://github.com/QiuSimons/vmlinux-btf.git package/vmlinux-btf
make package/vmlinux-btf/compile V=s
```

### In OpenWrt Buildroot

Select `vmlinux-btf` under `Kernel` in `make menuconfig`, then build:

```bash
make package/vmlinux-btf/compile V=s
```

## Installed Files

- `/usr/lib/debug/boot/vmlinux`: Detached BTF file.
- `/usr/lib/debug/boot/vmlinux-<release>`: Symlink targeting `vmlinux` (linked to `uname -r` on target installation).

Standard loaders such as `libbpf` look for BTF at `/usr/lib/debug/boot/vmlinux-$(uname -r)` and `/usr/lib/debug/boot/vmlinux` by default.

## License

GPL-3.0-or-later
