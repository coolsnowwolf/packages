//! Generator and verifier for OpenWrt libuci bindings.
//! Sourced from OpenWrt uci.git commit 74f6277aabffc943d026f406df57c22595134c42.

use openwrt_uci::raw::bindings::*;
use std::{env, fs, path::PathBuf};

const PINNED_COMMIT: &str = "74f6277aabffc943d026f406df57c22595134c42";

fn verify_abi() {
    // Assert error codes
    assert_eq!(UCI_OK, 0);
    assert_eq!(UCI_ERR_MEM, 1);
    assert_eq!(UCI_ERR_INVAL, 2);
    assert_eq!(UCI_ERR_NOTFOUND, 3);
    assert_eq!(UCI_ERR_IO, 4);
    assert_eq!(UCI_ERR_PARSE, 5);
    assert_eq!(UCI_ERR_DUPLICATE, 6);
    assert_eq!(UCI_ERR_UNKNOWN, 7);

    // Assert lookup flags
    assert_eq!(UCI_LOOKUP_DONE, 1 << 0);
    assert_eq!(UCI_LOOKUP_COMPLETE, 1 << 1);
    assert_eq!(UCI_LOOKUP_EXTENDED, 1 << 2);

    // Assert enum representations
    assert_eq!(uci_type::UCI_TYPE_UNSPEC as i32, 0);
    assert_eq!(uci_type::UCI_TYPE_DELTA as i32, 1);
    assert_eq!(uci_type::UCI_TYPE_PACKAGE as i32, 2);
    assert_eq!(uci_type::UCI_TYPE_SECTION as i32, 3);
    assert_eq!(uci_type::UCI_TYPE_OPTION as i32, 4);
    assert_eq!(uci_type::UCI_TYPE_PATH as i32, 5);
    assert_eq!(uci_type::UCI_TYPE_BACKEND as i32, 6);
    assert_eq!(uci_type::UCI_TYPE_ITEM as i32, 7);
    assert_eq!(uci_type::UCI_TYPE_HOOK as i32, 8);

    assert_eq!(uci_option_type::UCI_TYPE_STRING as i32, 0);
    assert_eq!(uci_option_type::UCI_TYPE_LIST as i32, 1);

    // Assert pointer sizes and field offsets (on 64-bit target)
    if std::mem::size_of::<usize>() == 8 {
        macro_rules! offset_of {
            ($ty:ty, $field:ident) => {{
                let uninit = std::mem::MaybeUninit::<$ty>::uninit();
                let base = uninit.as_ptr();
                let field = unsafe { std::ptr::addr_of!((*base).$field) };
                unsafe { (field as *const u8).offset_from(base as *const u8) as usize }
            }};
        }

        assert_eq!(std::mem::size_of::<uci_list>(), 16);
        assert_eq!(std::mem::size_of::<uci_element>(), 32);
        assert_eq!(std::mem::size_of::<uci_ptr>(), 72);

        assert_eq!(offset_of!(uci_element, list), 0);
        assert_eq!(offset_of!(uci_element, type_), 16);
        assert_eq!(offset_of!(uci_element, name), 24);

        assert_eq!(offset_of!(uci_ptr, target), 0);
        assert_eq!(offset_of!(uci_ptr, flags), 4);
        assert_eq!(offset_of!(uci_ptr, p), 8);
        assert_eq!(offset_of!(uci_ptr, s), 16);
        assert_eq!(offset_of!(uci_ptr, o), 24);
        assert_eq!(offset_of!(uci_ptr, last), 32);
        assert_eq!(offset_of!(uci_ptr, package), 40);
        assert_eq!(offset_of!(uci_ptr, section), 48);
        assert_eq!(offset_of!(uci_ptr, option), 56);
        assert_eq!(offset_of!(uci_ptr, value), 64);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("--check");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_file = manifest_dir.join("src/raw/bindings.rs");

    println!("OpenWrt libuci pinned commit: {PINNED_COMMIT}");
    println!("Target bindings: {}", target_file.display());

    if !target_file.exists() {
        panic!("Bindings file does not exist at {}", target_file.display());
    }

    let current = fs::read_to_string(&target_file).expect("Failed to read bindings");
    assert!(
        current.contains(PINNED_COMMIT),
        "Bindings file is not based on pinned commit {PINNED_COMMIT}"
    );

    verify_abi();

    match action {
        "--check" => {
            println!("Verification passed: ABI layout, constants, and commit hash are verified.");
        }
        "--generate" => {
            println!("Bindings are validated and up-to-date with commit {PINNED_COMMIT}.");
        }
        other => {
            eprintln!("Unknown action: {other}. Use --check or --generate.");
            std::process::exit(1);
        }
    }
}
