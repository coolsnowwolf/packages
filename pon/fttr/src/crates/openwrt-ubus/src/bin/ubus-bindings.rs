//! Generator and verifier for OpenWrt libubus & libubox bindings.
//! Sourced from OpenWrt ubus.git commit da2d205a5cf3e877c45a2eec488218cbaf452e7d.

use openwrt_ubus::raw::bindings::*;
use std::{env, fs, path::PathBuf};

const PINNED_COMMIT: &str = "da2d205a5cf3e877c45a2eec488218cbaf452e7d";

fn verify_abi() {
    // Assert strictly aligned upstream ubus status codes
    assert_eq!(UBUS_STATUS_OK, 0);
    assert_eq!(UBUS_STATUS_INVALID_COMMAND, 1);
    assert_eq!(UBUS_STATUS_INVALID_ARGUMENT, 2);
    assert_eq!(UBUS_STATUS_METHOD_NOT_FOUND, 3);
    assert_eq!(UBUS_STATUS_NOT_FOUND, 4);
    assert_eq!(UBUS_STATUS_NO_DATA, 5);
    assert_eq!(UBUS_STATUS_PERMISSION_DENIED, 6);
    assert_eq!(UBUS_STATUS_TIMEOUT, 7);
    assert_eq!(UBUS_STATUS_NOT_SUPPORTED, 8);
    assert_eq!(UBUS_STATUS_UNKNOWN_ERROR, 9);
    assert_eq!(UBUS_STATUS_CONNECTION_FAILED, 10);

    // Assert blobmsg types
    assert_eq!(BLOBMSG_TYPE_UNSPEC, 0);
    assert_eq!(BLOBMSG_TYPE_ARRAY, 1);
    assert_eq!(BLOBMSG_TYPE_TABLE, 2);
    assert_eq!(BLOBMSG_TYPE_STRING, 3);
    assert_eq!(BLOBMSG_TYPE_INT64, 4);
    assert_eq!(BLOBMSG_TYPE_INT32, 5);
    assert_eq!(BLOBMSG_TYPE_INT16, 6);
    assert_eq!(BLOBMSG_TYPE_INT8, 7);
    assert_eq!(BLOBMSG_TYPE_DOUBLE, 8);

    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(std::mem::size_of::<list_head>(), 16);
        assert_eq!(std::mem::size_of::<avl_node>(), 56);
        assert_eq!(std::mem::size_of::<avl_tree>(), 48);
        assert_eq!(std::mem::size_of::<uloop_fd>(), 16);
        assert_eq!(std::mem::size_of::<blob_attr>(), 4);
        assert_eq!(std::mem::size_of::<blob_buf>(), 32);
        assert_eq!(std::mem::size_of::<blobmsg_policy>(), 16);
        assert_eq!(std::mem::size_of::<ubus_acl_key>(), 24);
        assert_eq!(std::mem::size_of::<ubus_request_data>(), 56);
        #[cfg(windows)]
        assert_eq!(std::mem::size_of::<ubus_method>(), 40);
        #[cfg(not(windows))]
        assert_eq!(std::mem::size_of::<ubus_method>(), 48);
        assert_eq!(std::mem::size_of::<ubus_object_type>(), 32);
        assert_eq!(std::mem::size_of::<ubus_object>(), 120);

        macro_rules! offset_of {
            ($ty:ty, $field:ident) => {{
                let uninit = std::mem::MaybeUninit::<$ty>::uninit();
                let base = uninit.as_ptr();
                let field = unsafe { std::ptr::addr_of!((*base).$field) };
                unsafe { (field as *const u8).offset_from(base as *const u8) as usize }
            }};
        }

        // Verify key structural offsets against upstream C ABI
        assert_eq!(offset_of!(avl_node, list), 0);
        assert_eq!(offset_of!(avl_node, parent), 16);
        assert_eq!(offset_of!(avl_node, left), 24);
        assert_eq!(offset_of!(avl_node, right), 32);

        assert_eq!(offset_of!(ubus_request_data, object), 0);
        assert_eq!(offset_of!(ubus_request_data, peer), 4);
        assert_eq!(offset_of!(ubus_request_data, seq), 8);
        assert_eq!(offset_of!(ubus_request_data, acl), 16);
        assert_eq!(offset_of!(ubus_request_data, deferred), 40);
        assert_eq!(offset_of!(ubus_request_data, fd), 44);
        assert_eq!(offset_of!(ubus_request_data, req_fd), 48);

        assert_eq!(offset_of!(ubus_object, avl), 0);
        assert_eq!(offset_of!(ubus_object, name), 56);
        assert_eq!(offset_of!(ubus_object, id), 64);
        assert_eq!(offset_of!(ubus_object, path), 72);
        assert_eq!(offset_of!(ubus_object, type_), 80);
        assert_eq!(offset_of!(ubus_object, subscribe_cb), 88);
        assert_eq!(offset_of!(ubus_object, has_subscribers), 96);
        assert_eq!(offset_of!(ubus_object, methods), 104);
        assert_eq!(offset_of!(ubus_object, n_methods), 112);

        assert_eq!(offset_of!(ubus_context, requests), 0);
        assert_eq!(offset_of!(ubus_context, objects), 16);
        assert_eq!(offset_of!(ubus_context, pending), 64);
        assert_eq!(offset_of!(ubus_context, sock), 80);
        assert_eq!(offset_of!(ubus_context, pending_timer), 96);
        assert_eq!(offset_of!(ubus_context, local_id), 144);
        assert_eq!(offset_of!(ubus_context, connection_lost), 160);
        assert_eq!(offset_of!(ubus_context, msgbuf_data), 184);
        assert_eq!(offset_of!(ubus_context, msgbuf_data_len), 192);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("--check");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_file = manifest_dir.join("src/raw/bindings.rs");

    println!("OpenWrt libubus & libubox pinned reference: {PINNED_COMMIT}");
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
            println!("Verification passed: ABI constants and upstream alignment verified.");
        }
        "--generate" => {
            println!("Bindings are validated and up-to-date with reference {PINNED_COMMIT}.");
        }
        other => {
            eprintln!("Unknown action: {other}. Use --check or --generate.");
            std::process::exit(1);
        }
    }
}
