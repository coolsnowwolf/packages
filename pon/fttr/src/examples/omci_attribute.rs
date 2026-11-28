//! Private field diagnostic through the daemon's serialized ONU worker.
fn main() -> std::io::Result<()> {
    let command = std::env::args().nth(1).ok_or_else(|| std::io::Error::other(
        "JSON {onu_id,serial,generation,class,instance,mask[,expected:[bytes],replacement:[bytes]]}"))?;
    let _: serde_json::Value = serde_json::from_str(&command).map_err(std::io::Error::other)?;
    println!(
        "{}",
        openwrt_fttr::ipc::send_ipc_command(&format!("OMCI_ATTRIBUTE {command}"))?
    );
    Ok(())
}
