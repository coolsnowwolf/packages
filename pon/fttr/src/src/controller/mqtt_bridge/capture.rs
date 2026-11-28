use super::evidence::{attach_onu_capture_filter, decode_frame, decode_mqtt_syn, MqttSyn};
use super::reconcile::{now_secs, observe_dhcp_payload, observe_syn};
use crate::platform::openwrt::packet::PacketSocket;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct SynObservation {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub observed_at: Instant,
    pub epoch_secs: u64,
}

pub(super) struct CaptureControl {
    pub stop: Arc<AtomicBool>,
    pub handle: Option<std::thread::JoinHandle<()>>,
    pub desired_interfaces: BTreeMap<u8, String>,
}

pub(super) fn stop_capture(capture: &mut Option<CaptureControl>) {
    if let Some(mut cap) = capture.take() {
        cap.stop.store(true, Ordering::Release);
        if let Some(handle) = cap.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn ensure_capture(
    capture: &mut Option<CaptureControl>,
    enabled: bool,
    target_interfaces: BTreeMap<u8, String>,
) {
    if !enabled {
        stop_capture(capture);
        return;
    }
    if let Some(cap) = capture.as_ref() {
        if cap.desired_interfaces == target_interfaces
            && cap.handle.as_ref().is_some_and(|h| !h.is_finished())
        {
            return;
        }
    }
    stop_capture(capture);
    if target_interfaces.is_empty() {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let interfaces = target_interfaces.clone();
    let handle = std::thread::Builder::new()
        .name("mqtt-bridge-cap".into())
        .stack_size(128 * 1024)
        .spawn(move || run_capture_loop(interfaces, stop_flag))
        .ok();
    *capture = Some(CaptureControl {
        stop,
        handle,
        desired_interfaces: target_interfaces,
    });
}

fn run_capture_loop(interfaces: BTreeMap<u8, String>, stop: Arc<AtomicBool>) {
    let mut sockets: Vec<(String, PacketSocket)> = Vec::new();
    for iface in interfaces.values() {
        match PacketSocket::open_filtered(iface, false, |fd| attach_onu_capture_filter(fd)) {
            Ok(sock) => sockets.push((iface.clone(), sock)),
            Err(err) => eprintln!("[-] MQTT bridge capture unavailable on {iface}: {err}"),
        }
    }
    if sockets.is_empty() {
        return;
    }
    let mut pollfds: Vec<libc::pollfd> = sockets
        .iter()
        .map(|(_, s)| libc::pollfd {
            fd: s.raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let mut buffer = [0u8; 4096];
    while !stop.load(Ordering::Acquire) {
        let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, 500) };
        if ret <= 0 {
            continue;
        }
        for (idx, pfd) in pollfds.iter_mut().enumerate() {
            if pfd.revents & libc::POLLNVAL != 0 {
                return;
            }
            if pfd.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) == 0 {
                continue;
            }
            let has_err = pfd.revents & (libc::POLLERR | libc::POLLHUP) != 0;
            pfd.revents = 0;
            let (iface, sock) = &sockets[idx];
            loop {
                match sock.receive_into(&mut buffer) {
                    Ok(Some((bytes, kind, _vlan))) => {
                        let outgoing = kind == 4;
                        let now_inst = Instant::now();
                        let epoch = now_secs();
                        if let Some(dhcp) = decode_frame(bytes) {
                            if (dhcp.request && !outgoing) || (!dhcp.request && outgoing) {
                                observe_dhcp_payload(dhcp.payload, dhcp.source, now_inst);
                            }
                        } else if !outgoing {
                            if let Some(syn) = decode_mqtt_syn(bytes) {
                                handle_syn(iface, syn, now_inst, epoch);
                            }
                        }
                    }
                    Ok(None) => {
                        if has_err {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        break;
                    }
                    Err(e) => {
                        eprintln!("[-] MQTT bridge capture read error on {iface}: {e}");
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::ENODEV | libc::ENXIO | libc::EBADF)
                        ) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        break;
                    }
                }
            }
        }
    }
}

fn handle_syn(iface: &str, syn: MqttSyn, now_inst: Instant, epoch_secs: u64) {
    observe_syn(iface, syn, now_inst, epoch_secs);
}
