# vmlinux-btf

An OpenWrt package that builds a BTF-enabled `vmlinux` in a shadow kernel tree and installs it as a standalone file, enabling CO-RE (Compile Once – Run Everywhere) eBPF programs to run on OpenWrt without tainting or modifying the main kernel build.

---

## How It Works

1. Downloads the upstream Linux kernel source that matches the current OpenWrt build environment.
2. Extracts it into an isolated `shadow-kernel` directory inside the build tree.
3. Applies the same generic and platform-specific patches that the main kernel receives, in the same order.
4. Copies the main kernel's `.config` as a baseline, then enables the full set of BTF and BPF-related options on top of it.
5. Compiles `vmlinux` with full DWARF debug info.
6. Uses `pahole` to extract a detached BTF blob from the resulting `vmlinux`.
7. Installs the blob to `/usr/lib/debug/boot/vmlinux` on the target device.

This approach keeps the main kernel tree completely untouched — BTF information is generated entirely in the shadow build.

---

## Host Build Dependencies

The following tools must be present on the **host machine** running the OpenWrt build system before compiling this package.

### Required by the OpenWrt build system itself

These are standard OpenWrt build prerequisites. If your environment already builds OpenWrt successfully, these should already be present.

| Tool | Purpose |
|------|---------|
| `gcc` / `binutils` | Cross-compilation toolchain |
| `make` | Build orchestration |
| `xz-utils` | Decompression of kernel source tarball (`xzcat`) |
| `tar` | Extraction of kernel source |
| `bc` | Kernel version arithmetic during `prepare` |
| `flex`, `bison` | Kernel parser generation |
| `libssl-dev` | Kernel certificate handling |
| `libelf-dev` | ELF binary manipulation, required by kernel build |
| `python3` | Kernel build scripts |

Install on Debian/Ubuntu:
```bash
sudo apt install build-essential libssl-dev libelf-dev \
    bc flex bison python3 xz-utils tar
```

Install on Fedora/RHEL:
```bash
sudo dnf install gcc binutils openssl-devel elfutils-libelf-devel \
    bc flex bison python3 xz tar
```

---

### Required specifically by this package

#### `dwarves` (provides `pahole`)

`pahole` is used to extract the detached BTF blob from the compiled `vmlinux`. This is the only tool required by this package that is **not** a standard OpenWrt build dependency.

Install on Debian/Ubuntu:
```bash
sudo apt install dwarves
```

Install on Fedora/RHEL:
```bash
sudo dnf install dwarves
```

> **Note:** A host `pahole` version of **1.16 or newer** is required for `--btf_encode_detached` support. The version bundled with the OpenWrt `dwarves/host` feed satisfies this requirement. If you install `pahole` manually from your system package manager, verify the version with `pahole --version`.

---

## Building

Add this package to your OpenWrt build configuration:

```bash
# From the OpenWrt build root
make menuconfig
# Navigate to: Kernel → vmlinux-btf
# Select with <M> or <*>

make package/vmlinux-btf/compile V=s
```

Or build the IPK directly:

```bash
make package/vmlinux-btf/{clean,compile} V=s
```

---

## Installed Files

| Path | Description |
|------|-------------|
| `/usr/lib/debug/boot/vmlinux` | Detached BTF blob |
| `/usr/lib/debug/boot/vmlinux-<kernel-version>` | Symlink created by `postinst` |

The symlink is created at install time using the running kernel version (`uname -r`) and removed automatically on package removal.

---

## Notes

- **Build time is significant.** This package compiles a complete kernel from source. Expect 20–60 minutes depending on your hardware and target architecture.
- **Kernel version must match.** The shadow kernel is built from the same source version as the OpenWrt target kernel (`$(LINUX_VERSION)`). Installing this package on a device running a different kernel version will likely result in BTF type mismatches and broken CO-RE lookups.
- **Architecture-specific JIT support** (`HAVE_EBPF_JIT`) is set automatically by the kernel build system based on the target architecture and does not need to be configured manually.

---

## License

GPL-3.0-or-later — see [https://www.gnu.org/licenses/gpl-3.0.html](https://www.gnu.org/licenses/gpl-3.0.html)
