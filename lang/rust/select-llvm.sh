#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
#
# Select a compatible host LLVM toolchain or compute minimal on-demand /
# incremental bundled LLVM targets for Rust bootstrap configuration.

set -eu

SRC_DIR=""
BUILD_DIR=""
HOST_TRIPLE=""
NEEDED_TARGETS=""
USE_HOST_LLVM="y"
DOWNLOAD_CI_LLVM="n"
HOSTCXX="${HOSTCXX:-c++}"
OUT_ARGS=""
PROBE_DIR=""

cleanup() {
	if [ -n "$PROBE_DIR" ] && [ -d "$PROBE_DIR" ]; then
		rm -rf "$PROBE_DIR"
	fi
}
trap cleanup EXIT INT TERM

log_info() {
	printf 'rust/select-llvm: %s\n' "$*" >&2
}

die() {
	printf 'rust/select-llvm: ERROR: %s\n' "$*" >&2
	exit 1
}

while [ "$#" -gt 0 ]; do
	case "$1" in
		--src-dir)
			[ "$#" -ge 2 ] || die "missing value for $1"
			SRC_DIR="$2"
			shift 2
			;;
		--build-dir)
			[ "$#" -ge 2 ] || die "missing value for $1"
			BUILD_DIR="$2"
			shift 2
			;;
		--host-triple)
			[ "$#" -ge 2 ] || die "missing value for $1"
			HOST_TRIPLE="$2"
			shift 2
			;;
		--needed-targets)
			[ "$#" -ge 2 ] || die "missing value for $1"
			NEEDED_TARGETS="$2"
			shift 2
			;;
		--use-host-llvm)
			[ "$#" -ge 2 ] || die "missing value for $1"
			USE_HOST_LLVM="$2"
			shift 2
			;;
		--download-ci-llvm)
			[ "$#" -ge 2 ] || die "missing value for $1"
			DOWNLOAD_CI_LLVM="$2"
			shift 2
			;;
		--hostcxx)
			[ "$#" -ge 2 ] || die "missing value for $1"
			if [ -n "$2" ]; then
				HOSTCXX="$2"
			fi
			shift 2
			;;
		--out-args)
			[ "$#" -ge 2 ] || die "missing value for $1"
			OUT_ARGS="$2"
			shift 2
			;;
		*)
			die "unknown option: $1"
			;;
	esac
done

[ -n "$SRC_DIR" ] || die "--src-dir is required"
[ -n "$BUILD_DIR" ] || die "--build-dir is required"
[ -n "$HOST_TRIPLE" ] || die "--host-triple is required"

RUSTFLAGS_FILE="${BUILD_DIR}/.host-llvm-rustflags"
rm -f "$RUSTFLAGS_FILE"

emit_args() {
	if [ -n "$OUT_ARGS" ]; then
		: > "$OUT_ARGS"
		for arg in "$@"; do
			printf '%s\n' "$arg" >> "$OUT_ARGS"
		done
	else
		for arg in "$@"; do
			printf '%s\n' "$arg"
		done
	fi
}

# 1. Dynamically extract supported LLVM major version window [MIN_MAJOR..MAX_MAJOR].
LLVM_VER_CMAKE="${SRC_DIR}/src/llvm-project/cmake/Modules/LLVMVersion.cmake"
[ -f "$LLVM_VER_CMAKE" ] || die "missing $LLVM_VER_CMAKE"

MAX_MAJOR="$(awk '
	{
		sub(/#.*/, "")
		if (match($0, /set[[:space:]]*\([[:space:]]*LLVM_VERSION_MAJOR[[:space:]]+"?[0-9]+"?[[:space:]]*\)/)) {
			s = substr($0, RSTART, RLENGTH)
			gsub(/^.*LLVM_VERSION_MAJOR[[:space:]]+"?/, "", s)
			gsub(/"?[[:space:]]*\).*$/, "", s)
			val = s
			exit
		}
	}
	END { if (val != "") print val }
' "$LLVM_VER_CMAKE")"
[ -n "$MAX_MAJOR" ] || die "failed to extract LLVM_VERSION_MAJOR from $LLVM_VER_CMAKE"

LLVM_RS=""
if [ -f "${SRC_DIR}/src/bootstrap/src/core/build_steps/llvm.rs" ]; then
	LLVM_RS="${SRC_DIR}/src/bootstrap/src/core/build_steps/llvm.rs"
elif [ -f "${SRC_DIR}/src/bootstrap/llvm.rs" ]; then
	LLVM_RS="${SRC_DIR}/src/bootstrap/llvm.rs"
fi

MIN_MAJOR=""
if [ -n "$LLVM_RS" ]; then
	MIN_MAJOR="$(awk '
		/fn[[:space:]]+check_llvm_version/ { in_fn = 1 }
		in_fn {
			if (match($0, /major[[:space:]]*>=[[:space:]]*[0-9]+/)) {
				s = substr($0, RSTART, RLENGTH)
				sub(/^.*>=[[:space:]]*/, "", s)
				val = s
			} else if (match($0, /major[[:space:]]*<[[:space:]]*[0-9]+/)) {
				s = substr($0, RSTART, RLENGTH)
				sub(/^.*<[[:space:]]*/, "", s)
				val = s
			} else if (match($0, /need[[:space:]]*>=[[:space:]]*[0-9]+/)) {
				s = substr($0, RSTART, RLENGTH)
				sub(/^.*>=[[:space:]]*/, "", s)
				val = s
			}
			if (/^\}/ && val != "") exit
		}
		END { if (val != "") print val }
	' "$LLVM_RS")"
fi

if [ -z "$MIN_MAJOR" ]; then
	log_info "warning: could not parse MIN_MAJOR from llvm.rs; defaulting MIN_MAJOR=$MAX_MAJOR"
	MIN_MAJOR="$MAX_MAJOR"
fi

if [ "$MIN_MAJOR" -gt "$MAX_MAJOR" ]; then
	die "invalid LLVM major window [$MIN_MAJOR..$MAX_MAJOR]"
fi

# 2. Dynamically parse core and experimental LLVM targets from CMakeLists.txt.
LLVM_CMAKE="${SRC_DIR}/src/llvm-project/llvm/CMakeLists.txt"
[ -f "$LLVM_CMAKE" ] || die "missing $LLVM_CMAKE"

parse_cmake_target_list() {
	var_name="$1"
	awk -v target_var="$var_name" '
		{
			sub(/#.*/, "")
			line = $0
			if (!in_set) {
				pat = "set[[:space:]]*\\([[:space:]]*" target_var "([[:space:]]|\\)|$)"
				if (match(line, pat)) {
					in_set = 1
					matched = substr(line, RSTART, RLENGTH)
					tail = substr(line, RSTART + RLENGTH)
					if (substr(matched, length(matched), 1) == ")") {
						in_set = 0
						done = 1
						line = ""
					} else {
						line = tail
					}
				} else {
					next
				}
			}
			if (in_set) {
				if (index(line, ")") > 0) {
					sub(/\).*/, "", line)
					in_set = 0
					done = 1
				}
				gsub(/[[:space:]]+/, " ", line)
				gsub(/^ | $/, "", line)
				if (line != "") {
					out = (out == "" ? line : out " " line)
				}
				if (done) exit
			}
		}
		END { if (out != "") print out }
	' "$LLVM_CMAKE"
}

CORE_TARGETS="$(parse_cmake_target_list "LLVM_ALL_TARGETS")"
EXP_TARGETS="$(parse_cmake_target_list "LLVM_ALL_EXPERIMENTAL_TARGETS")"
[ -n "$CORE_TARGETS" ] || die "failed to parse LLVM_ALL_TARGETS from $LLVM_CMAKE"

is_in_space_list() {
	needle="$1"
	haystack="$2"
	case " $haystack " in
		*" $needle "*) return 0 ;;
		*) return 1 ;;
	esac
}

append_unique_space() {
	item="$1"
	list="$2"
	if [ -z "$item" ]; then
		printf '%s' "$list"
		return 0
	fi
	if is_in_space_list "$item" "$list"; then
		printf '%s' "$list"
	elif [ -z "$list" ]; then
		printf '%s' "$item"
	else
		printf '%s %s' "$list" "$item"
	fi
}

lookup_case_insensitive() {
	lower_tok="$1"
	candidates="$2"
	for cand in $candidates; do
		cand_lower="$(printf '%s' "$cand" | tr '[:upper:]' '[:lower:]')"
		if [ "$lower_tok" = "$cand_lower" ]; then
			printf '%s' "$cand"
			return 0
		fi
	done
	return 1
}

normalize_target_token() {
	raw="$1"
	lower="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"
	case "$lower" in
		all|all_core|all-core)
			printf '%s' "ALL_CORE"
			return 0
			;;
		x86|x86_64|amd64|i386|i486|i586|i686)
			printf '%s' "X86"
			return 0
			;;
		aarch64|aarch64_be|arm64)
			printf '%s' "AArch64"
			return 0
			;;
		arm|armeb|armv*)
			printf '%s' "ARM"
			return 0
			;;
		mips|mipsel|mips64|mips64el)
			printf '%s' "Mips"
			return 0
			;;
		powerpc|powerpc64|powerpc64le|ppc*)
			printf '%s' "PowerPC"
			return 0
			;;
		riscv|riscv32|riscv64|riscv*)
			printf '%s' "RISCV"
			return 0
			;;
		loongarch|loongarch32|loongarch64|loongarch*)
			printf '%s' "LoongArch"
			return 0
			;;
		sparc|sparc64|sparcv9)
			printf '%s' "Sparc"
			return 0
			;;
		s390|s390x|systemz)
			printf '%s' "SystemZ"
			return 0
			;;
		arc|arceb|archs*)
			printf '%s' "ARC"
			return 0
			;;
		csky)
			printf '%s' "CSKY"
			return 0
			;;
		m68k)
			printf '%s' "M68k"
			return 0
			;;
		xtensa)
			printf '%s' "Xtensa"
			return 0
			;;
		avr|avr32)
			printf '%s' "AVR"
			return 0
			;;
		msp430)
			printf '%s' "MSP430"
			return 0
			;;
		hexagon)
			printf '%s' "Hexagon"
			return 0
			;;
		lanai)
			printf '%s' "Lanai"
			return 0
			;;
		ve)
			printf '%s' "VE"
			return 0
			;;
		xcore)
			printf '%s' "XCore"
			return 0
			;;
		bpf|bpfeb|bpfel)
			printf '%s' "BPF"
			return 0
			;;
		wasm|wasm32|wasm64|webassembly)
			printf '%s' "WebAssembly"
			return 0
			;;
		amdgpu|amdgcn)
			printf '%s' "AMDGPU"
			return 0
			;;
		nvptx|nvptx64)
			printf '%s' "NVPTX"
			return 0
			;;
		spirv|spirv32|spirv64)
			printf '%s' "SPIRV"
			return 0
			;;
		directx|dxil)
			printf '%s' "DirectX"
			return 0
			;;
	esac

	if matched="$(lookup_case_insensitive "$lower" "$CORE_TARGETS")"; then
		printf '%s' "$matched"
		return 0
	fi
	if matched="$(lookup_case_insensitive "$lower" "$EXP_TARGETS")"; then
		printf '%s' "$matched"
		return 0
	fi
	return 1
}

expand_target_list() {
	raw_list="$(printf '%s' "$1" | tr ',;\t\n' '    ')"
	acc=""
	for tok in $raw_list; do
		[ -n "$tok" ] || continue
		norm="$(normalize_target_token "$tok")" || die "unknown LLVM target '$tok'"
		if [ "$norm" = "ALL_CORE" ]; then
			for core_t in $CORE_TARGETS; do
				acc="$(append_unique_space "$core_t" "$acc")"
			done
		else
			if ! is_in_space_list "$norm" "$CORE_TARGETS" && ! is_in_space_list "$norm" "$EXP_TARGETS"; then
				die "LLVM target '$norm' (from '$tok') is not listed in $LLVM_CMAKE"
			fi
			acc="$(append_unique_space "$norm" "$acc")"
		fi
	done
	printf '%s' "$acc"
}

REQ_TARGETS="$(expand_target_list "$NEEDED_TARGETS")"
if [ -z "$REQ_TARGETS" ]; then
	host_m="$(uname -m 2>/dev/null || true)"
	if [ -n "$host_m" ] && norm_m="$(normalize_target_token "$host_m" 2>/dev/null)"; then
		if [ "$norm_m" != "ALL_CORE" ]; then
			REQ_TARGETS="$norm_m"
		fi
	fi
fi
[ -n "$REQ_TARGETS" ] || die "could not determine any required LLVM target"

# 3. Tier 1: Host LLVM 6-point probe.
probe_host_llvm_candidate() {
	cfg="$1"

	if [ ! -x "$cfg" ]; then
		log_info "skipping $cfg: not executable"
		return 1
	fi

	# Point 1: major version in [MIN_MAJOR..MAX_MAJOR].
	llvm_ver="$("$cfg" --version 2>/dev/null || true)"
	llvm_major="$(printf '%s\n' "$llvm_ver" | sed -n 's/^\([0-9][0-9]*\).*/\1/p' | head -n 1)"
	if [ -z "$llvm_major" ] || [ "$llvm_major" -lt "$MIN_MAJOR" ] || [ "$llvm_major" -gt "$MAX_MAJOR" ]; then
		log_info "skipping $cfg: LLVM version '${llvm_ver:-unknown}' outside supported [$MIN_MAJOR..$MAX_MAJOR]"
		return 1
	fi

	# Point 2: Core.h header exists.
	incdir="$("$cfg" --includedir 2>/dev/null || true)"
	if [ -z "$incdir" ] || [ ! -f "$incdir/llvm-c/Core.h" ]; then
		log_info "skipping $cfg: missing ${incdir:-<empty>}/llvm-c/Core.h"
		return 1
	fi

	# Point 3: required target backends built.
	targets_built="$(printf '%s' "$("$cfg" --targets-built 2>/dev/null || true)" | tr '\t\n' '  ')"
	missing_targets=""
	for req_t in $REQ_TARGETS; do
		if ! is_in_space_list "$req_t" "$targets_built"; then
			missing_targets="${missing_targets:+$missing_targets }$req_t"
		fi
	done
	if [ -n "$missing_targets" ]; then
		log_info "skipping $cfg: missing required LLVM targets: $missing_targets"
		return 1
	fi

	# Point 4: all 8 required components present.
	comps="$(printf '%s' "$("$cfg" --components 2>/dev/null || true)" | tr '\t\n' '  ')"
	missing_comps=""
	for req_c in ipo bitreader bitwriter linker asmparser lto coverage instrumentation; do
		if ! is_in_space_list "$req_c" "$comps"; then
			missing_comps="${missing_comps:+$missing_comps }$req_c"
		fi
	done
	if [ -n "$missing_comps" ]; then
		log_info "skipping $cfg: missing required LLVM components: $missing_comps"
		return 1
	fi

	# Point 5: Real HOSTCXX compile + link + execute probe calling LLVMShutdown.
	libdir="$("$cfg" --libdir 2>/dev/null || true)"
	bindir="$("$cfg" --bindir 2>/dev/null || true)"
	if [ -z "$libdir" ]; then
		log_info "skipping $cfg: empty --libdir"
		return 1
	fi

	if [ -z "$PROBE_DIR" ]; then
		mkdir -p "$BUILD_DIR"
		PROBE_DIR="${BUILD_DIR}/.llvm-probe-$$"
		mkdir -p "$PROBE_DIR"
	fi

	probe_cpp="${PROBE_DIR}/probe.cpp"
	probe_bin="${PROBE_DIR}/probe"
	cat > "$probe_cpp" <<-'EOF'
	#include <llvm-c/Core.h>
	int main() {
		LLVMShutdown();
		return 0;
	}
	EOF

	filecheck_arg=""
	if [ -n "$bindir" ] && [ -x "$bindir/FileCheck" ]; then
		filecheck_arg="--llvm-filecheck=$bindir/FileCheck"
	fi

	shared_libs="$("$cfg" --link-shared --libs 2>/dev/null || true)"
	if [ -n "$shared_libs" ]; then
		# 5a: shared linking without extra RPATH.
		rm -f "$probe_bin"
		# shellcheck disable=SC2086
		if $HOSTCXX -I"$incdir" "$probe_cpp" -L"$libdir" $shared_libs -o "$probe_bin" >/dev/null 2>&1 \
			&& "$probe_bin" >/dev/null 2>&1; then
			rm -f "$RUSTFLAGS_FILE"
			log_info "using host shared LLVM ($cfg, version $llvm_ver, no extra rpath)"
			if [ -n "$filecheck_arg" ]; then
				emit_args "--llvm-config=$cfg" "--enable-llvm-link-shared" "--set=llvm.download-ci-llvm=false" "$filecheck_arg" "--disable-codegen-tests"
			else
				emit_args "--llvm-config=$cfg" "--enable-llvm-link-shared" "--set=llvm.download-ci-llvm=false" "--disable-codegen-tests"
			fi
			return 0
		fi

		# 5b: shared linking with -Wl,-rpath,$libdir.
		rm -f "$probe_bin"
		# shellcheck disable=SC2086
		if $HOSTCXX -I"$incdir" "$probe_cpp" -L"$libdir" "-Wl,-rpath,$libdir" $shared_libs -o "$probe_bin" >/dev/null 2>&1 \
			&& "$probe_bin" >/dev/null 2>&1; then
			printf '%s\n' "-C link-arg=-Wl,-rpath,$libdir" > "$RUSTFLAGS_FILE"
			log_info "using host shared LLVM ($cfg, version $llvm_ver, rpath=$libdir)"
			if [ -n "$filecheck_arg" ]; then
				emit_args "--llvm-config=$cfg" "--enable-llvm-link-shared" "--set=llvm.download-ci-llvm=false" "$filecheck_arg" "--disable-codegen-tests"
			else
				emit_args "--llvm-config=$cfg" "--enable-llvm-link-shared" "--set=llvm.download-ci-llvm=false" "--disable-codegen-tests"
			fi
			return 0
		fi
	fi

	# 5c: static linking fallback.
	static_libs="$("$cfg" --link-static --libs --system-libs 2>/dev/null || true)"
	ldflags="$("$cfg" --ldflags 2>/dev/null || true)"
	if [ -n "$static_libs" ]; then
		rm -f "$probe_bin"
		# shellcheck disable=SC2086
		if $HOSTCXX -I"$incdir" "$probe_cpp" -L"$libdir" $ldflags $static_libs -o "$probe_bin" >/dev/null 2>&1 \
			&& "$probe_bin" >/dev/null 2>&1; then
			rm -f "$RUSTFLAGS_FILE"
			log_info "using host static LLVM ($cfg, version $llvm_ver)"
			if [ -n "$filecheck_arg" ]; then
				emit_args "--llvm-config=$cfg" "--set=llvm.download-ci-llvm=false" "$filecheck_arg" "--disable-codegen-tests"
			else
				emit_args "--llvm-config=$cfg" "--set=llvm.download-ci-llvm=false" "--disable-codegen-tests"
			fi
			return 0
		fi
	fi

	log_info "skipping $cfg: link/runtime probe failed"
	return 1
}

normalize_candidate_path() {
	p="$1"
	d="$(dirname "$p")"
	b="$(basename "$p")"
	if [ -d "$d" ]; then
		(cd "$d" 2>/dev/null && printf '%s/%s' "$(pwd -P)" "$b") || printf '%s' "$p"
	else
		printf '%s' "$p"
	fi
}

if [ "$USE_HOST_LLVM" = "y" ]; then
	sys_root="${RUST_LLVM_SYS_ROOT:-}"
	seen_candidates=""

	try_candidate() {
		cand="$1"
		[ -n "$cand" ] || return 1
		norm_cand="$(normalize_candidate_path "$cand")"
		case "
$seen_candidates
" in
			*"
$norm_cand
"*)
				return 1
				;;
		esac
		seen_candidates="${seen_candidates}
${norm_cand}"
		probe_host_llvm_candidate "$cand"
	}

	if [ -n "${RUST_HOST_LLVM_CONFIG:-}" ]; then
		if try_candidate "$RUST_HOST_LLVM_CONFIG"; then
			exit 0
		fi
	fi

	v="$MAX_MAJOR"
	while [ "$v" -ge "$MIN_MAJOR" ]; do
		for cand in \
			"${sys_root}/usr/lib/llvm-${v}/bin/llvm-config" \
			"${sys_root}/opt/homebrew/opt/llvm@${v}/bin/llvm-config" \
			"${sys_root}/usr/local/opt/llvm@${v}/bin/llvm-config"
		do
			if [ -e "$cand" ] || [ -L "$cand" ]; then
				if try_candidate "$cand"; then
					exit 0
				fi
			fi
		done

		if path_v="$(command -v "llvm-config-${v}" 2>/dev/null || true)" && [ -n "$path_v" ]; then
			if try_candidate "$path_v"; then
				exit 0
			fi
		fi

		v=$((v - 1))
	done

	if path_plain="$(command -v llvm-config 2>/dev/null || true)" && [ -n "$path_plain" ]; then
		if try_candidate "$path_plain"; then
			exit 0
		fi
	fi

	log_info "no compatible host LLVM found; falling back to bundled LLVM"
else
	log_info "host LLVM disabled (CONFIG_RUST_USE_HOST_LLVM=n); using bundled LLVM"
fi

# 4. Tier 2/3: On-demand & incremental bundled LLVM fallback.
rm -f "$RUSTFLAGS_FILE"

CACHE_FILE="${BUILD_DIR}/build/${HOST_TRIPLE}/llvm/build/CMakeCache.txt"
PREV_BUILT=""
if [ -f "$CACHE_FILE" ]; then
	raw_prev="$(awk '
		/^[[:space:]]*LLVM_(TARGETS|EXPERIMENTAL_TARGETS)_TO_BUILD:[A-Za-z0-9_]+=/ {
			val = substr($0, index($0, "=") + 1)
			gsub(/;/, " ", val)
			if (val != "" && val !~ /-NOTFOUND$/) {
				out = (out == "" ? val : out " " val)
			}
		}
		END { if (out != "") print out }
	' "$CACHE_FILE")"
	if [ -n "$raw_prev" ]; then
		PREV_BUILT="$(expand_target_list "$raw_prev")"
	fi
fi

UNION_TARGETS=""
for t in $PREV_BUILT $REQ_TARGETS; do
	UNION_TARGETS="$(append_unique_space "$t" "$UNION_TARGETS")"
done

CORE_LIST=""
EXP_LIST=""
for t in $UNION_TARGETS; do
	if is_in_space_list "$t" "$CORE_TARGETS"; then
		CORE_LIST="${CORE_LIST:+$CORE_LIST;}$t"
	elif is_in_space_list "$t" "$EXP_TARGETS"; then
		EXP_LIST="${EXP_LIST:+$EXP_LIST;}$t"
	else
		die "internal error: target '$t' is neither core nor experimental"
	fi
done

if [ "$DOWNLOAD_CI_LLVM" = "y" ] && [ -z "$EXP_LIST" ]; then
	if command -v curl >/dev/null 2>&1 \
		&& curl -fsSIL --connect-timeout 5 --max-time 5 https://static.rust-lang.org/ >/dev/null 2>&1; then
		log_info "using prebuilt CI LLVM (static.rust-lang.org reachable, no experimental targets)"
		emit_args "--set=llvm.download-ci-llvm=true" "--set=llvm.link-jobs=1"
		exit 0
	fi
	log_info "CI LLVM requested but https://static.rust-lang.org/ unreachable; building bundled LLVM"
elif [ "$DOWNLOAD_CI_LLVM" = "y" ] && [ -n "$EXP_LIST" ]; then
	log_info "experimental targets ($EXP_LIST) requested; skipping CI LLVM and building bundled LLVM"
fi

log_info "bundled LLVM targets: core='$CORE_LIST' experimental='$EXP_LIST'"
emit_args \
	"--set=llvm.download-ci-llvm=false" \
	"--set=llvm.targets=$CORE_LIST" \
	"--set=llvm.experimental-targets=$EXP_LIST" \
	"--set=llvm.link-jobs=1"
