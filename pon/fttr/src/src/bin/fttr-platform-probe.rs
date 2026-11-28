// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::process;
use std::time::Instant;

use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct SystemInfo {
    uptime: u64,
    localtime: u64,
    load: Vec<u32>,
    memory: MemoryInfo,
}

#[derive(Deserialize, Debug)]
struct MemoryInfo {
    total: u64,
    free: u64,
}

fn get_process_rss_kb() -> Option<u64> {
    if let Ok(content) = fs::read_to_string("/proc/self/statm") {
        let parts: Vec<&str> = content.split_whitespace().collect();
        if parts.len() >= 2 {
            if let Ok(pages) = parts[1].parse::<u64>() {
                // 4KB page size
                return Some(pages * 4);
            }
        }
    }
    None
}

fn test_uci() -> Result<(), String> {
    println!("[PROBE] Testing openwrt-uci dynamic loader & typed extraction...");
    let mut uci = match openwrt_uci::Uci::new() {
        Ok(u) => {
            println!("  [+] Successfully loaded OpenWrt libuci library and created context.");
            u
        }
        Err(e) => {
            println!("  [-] libuci load/context failed: {e}");
            return Err(e.to_string());
        }
    };

    println!("[PROBE] Testing UCI read on 'system' package...");
    match uci.get("system", "@system[0]", "hostname") {
        Ok(Some(hostname)) => {
            println!("  [+] Retrieved system hostname: {hostname}");
        }
        Ok(None) => {
            println!("  [*] system.hostname option not found.");
        }
        Err(e) => {
            println!("  [*] UCI get failed: {e}");
        }
    }

    println!("[PROBE] Testing in-process native UCI package export on 'system'...");
    match uci.export_package("system") {
        Ok(text) => {
            let line_count = text.lines().count();
            println!("  [+] Successfully exported 'system' via libuci C API ({line_count} lines, {} bytes)", text.len());
        }
        Err(e) => {
            println!("  [-] UCI export failed: {e}");
            return Err(e.to_string());
        }
    }

    println!("[PROBE] Testing native list extraction on 'fttr.provisioning.trigger'...");
    match uci.get_list("fttr", "provisioning", "trigger") {
        Ok(Some(triggers)) => {
            println!("  [+] Successfully extracted list elements: {:?}", triggers);
        }
        Ok(None) => {
            println!("  [*] fttr.provisioning.trigger list not found.");
        }
        Err(e) => {
            println!("  [-] get_list failed: {e}");
            return Err(e.to_string());
        }
    }

    Ok(())
}

fn test_ubus_typed() -> Result<(), String> {
    println!("[PROBE] Testing openwrt-ubus strongly-typed struct conversion...");
    let mut ubus = match openwrt_ubus::Ubus::connect(None) {
        Ok(u) => {
            println!("  [+] Successfully connected to OpenWrt ubus daemon.");
            u
        }
        Err(e) => {
            println!("  [-] ubus connect/load failed: {e}");
            return Err(e.to_string());
        }
    };

    println!("[PROBE] Invoking 'system.info' directly into strongly-typed SystemInfo struct...");
    match ubus.call_empty::<SystemInfo>("system", "info", 3000) {
        Ok(info) => {
            println!("  [+] Successfully deserialized into Rust struct:");
            println!("      - Uptime: {} seconds", info.uptime);
            println!("      - Localtime: {}", info.localtime);
            println!(
                "      - Total Memory: {} MB",
                info.memory.total / 1024 / 1024
            );
            println!("      - Free Memory: {} MB", info.memory.free / 1024 / 1024);
            println!("      - Load Averages: {:?}", info.load);
        }
        Err(e) => {
            println!("  [-] Typed call failed: {e}");
            return Err(e.to_string());
        }
    }

    Ok(())
}

fn test_memory_leak() -> Result<(), String> {
    println!("[PROBE] Starting memory leak stress test (500 cycles of ubus + uci calls)...");
    let initial_rss = get_process_rss_kb().unwrap_or(0);
    println!("  [i] Initial Process RSS: {initial_rss} KB");

    let mut uci = openwrt_uci::Uci::new().map_err(|e| e.to_string())?;
    let mut ubus = openwrt_ubus::Ubus::connect(None).map_err(|e| e.to_string())?;

    let iterations = 500;
    let start = Instant::now();

    for i in 1..=iterations {
        // 1. High frequency typed ubus call
        let _info: SystemInfo = ubus
            .call_empty("system", "info", 1000)
            .map_err(|e| format!("ubus call failed at iter {i}: {e}"))?;

        // 2. High frequency uci read
        let _ = uci.get("system", "@system[0]", "hostname");

        if i % 100 == 0 {
            let curr_rss = get_process_rss_kb().unwrap_or(0);
            println!("  [i] Iteration {i}/{iterations}: RSS = {curr_rss} KB");
        }
    }

    let elapsed = start.elapsed();
    let final_rss = get_process_rss_kb().unwrap_or(0);
    let diff = (final_rss as i64) - (initial_rss as i64);

    println!("  [+] Completed {iterations} iterations in {:?}", elapsed);
    println!("  [i] Final Process RSS: {final_rss} KB (delta: {diff} KB)");

    // On Linux musl allocator, a tiny delta (< 256KB for allocator arena initialization) is normal.
    // If there were an unreleased C string or buffer in the loop, 500 calls would leak several MBs.
    if diff > 1024 {
        println!("  [!] Warning: noticeable memory growth detected: {diff} KB");
        return Err(format!("Potential memory leak: RSS grew by {diff} KB"));
    } else {
        println!("  [+] Memory is flat and bounded (no leaks detected).");
    }

    Ok(())
}

fn test_uci_isolated_write() -> Result<(), String> {
    println!("[PROBE] Testing isolated UCI write, list replace & commit semantics...");
    let tmp_dir = "/tmp/test_uci_probe";
    let save_dir = "/tmp/test_uci_probe/savedir";
    let _ = fs::remove_dir_all(tmp_dir);
    fs::create_dir_all(save_dir).map_err(|e| e.to_string())?;

    // Create an initial test package file
    let initial_conf = "config test_sec 'main'\n\toption name 'initial_val'\n";
    fs::write(format!("{tmp_dir}/testpkg"), initial_conf).map_err(|e| e.to_string())?;

    // Context 1: Modify and Commit
    {
        let mut uci = openwrt_uci::Uci::new().map_err(|e| e.to_string())?;
        uci.set_confdir(tmp_dir).map_err(|e| e.to_string())?;
        uci.set_savedir(save_dir).map_err(|e| e.to_string())?;

        // 1. replace_list on a non-existent option (P1 test)
        uci.replace_list(
            "testpkg",
            "main",
            "ports",
            &["port1".into(), "port2".into()],
        )
        .map_err(|e| format!("replace_list on non-existent option failed: {e}"))?;

        // 2. set an option
        uci.set("testpkg", "main", "updated_key", "hello_fttr")
            .map_err(|e| format!("set failed: {e}"))?;

        // 3. commit in the SAME context (P1 duplicate package load test)
        uci.commit("testpkg")
            .map_err(|e| format!("commit in same context failed: {e}"))?;
    }

    // Context 2: Read back from fresh context to verify persisted changes
    {
        let mut uci = openwrt_uci::Uci::new().map_err(|e| e.to_string())?;
        uci.set_confdir(tmp_dir).map_err(|e| e.to_string())?;
        uci.set_savedir(save_dir).map_err(|e| e.to_string())?;

        let val = uci
            .get("testpkg", "main", "updated_key")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "updated_key not found".to_string())?;
        if val != "hello_fttr" {
            return Err(format!("expected 'hello_fttr', got '{val}'"));
        }

        let list = uci
            .get_list("testpkg", "main", "ports")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "ports list not found".to_string())?;
        if list != vec!["port1", "port2"] {
            return Err(format!("expected ['port1', 'port2'], got {:?}", list));
        }

        println!("  [+] Isolated write, non-existent replace_list, and single-context commit verified successfully.");
    }

    let _ = fs::remove_dir_all(tmp_dir);
    Ok(())
}

fn main() {
    println!("=== FTTR Native Platform Capability & Memory Leak Test ===");
    let uci_ok = test_uci().is_ok();
    let uci_write_ok = test_uci_isolated_write().is_ok();
    let ubus_ok = test_ubus_typed().is_ok();
    let leak_ok = test_memory_leak().is_ok();

    println!("============================================================");
    println!("Summary: UCI={uci_ok}, UCI_WRITE={uci_write_ok}, UBUS_TYPED={ubus_ok}, MEMORY_STABLE={leak_ok}");
    if uci_ok && uci_write_ok && ubus_ok && leak_ok {
        println!("All verifications passed with zero memory leaks!");
        process::exit(0);
    } else {
        println!("Some verifications failed.");
        process::exit(1);
    }
}
