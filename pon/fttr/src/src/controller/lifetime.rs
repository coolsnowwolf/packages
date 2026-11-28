use crate::ipc::SubDevice;
use std::sync::{Arc, Mutex};
#[cfg(feature = "mqtt")]
use std::time::Duration;

#[cfg(feature = "mqtt")]
mod lan;

/// Process-owned MQTT resources. Process termination reclaims upstream threads;
/// rumqttd does not expose a graceful shutdown handle.
pub(crate) struct Lifetime {
    #[cfg(feature = "mqtt")]
    broker: crate::broker::EmbeddedBroker,
    #[cfg(feature = "mqtt")]
    address: std::net::SocketAddr,
    #[cfg(feature = "mqtt")]
    lan: Option<std::net::Ipv4Addr>,
}

impl Lifetime {
    pub(crate) async fn start() -> Result<Self, String> {
        #[cfg(feature = "mqtt")]
        {
            let lan = lan::wait_for_address().await;
            let addresses = crate::broker::lan_addresses(&lan.to_string())?;
            let broker = crate::broker::EmbeddedBroker::start(&addresses).await?;
            super::mqtt_bridge::set_broker_address(Some(lan));
            println!("[+] Owned MQTT broker ready on {addresses:?}");
            Ok(Self {
                broker,
                address: addresses[0],
                lan: Some(lan),
            })
        }
        #[cfg(not(feature = "mqtt"))]
        {
            Ok(Self {})
        }
    }

    pub(crate) async fn wait(self, devices: Arc<Mutex<Vec<SubDevice>>>) -> Result<(), String> {
        #[cfg(feature = "mqtt")]
        {
            let mut broker = self.broker;
            let telemetry = super::mqtt::run(self.address, devices);
            tokio::pin!(telemetry);
            let mut lan_interval = tokio::time::interval(Duration::from_secs(3));
            let mut health_interval = tokio::time::interval(Duration::from_secs(10));
            lan_interval.tick().await;
            health_interval.tick().await;
            loop {
                tokio::select! {
                    result = &mut telemetry => return Err(format!("telemetry worker stopped: {result:?}")),
                    _ = health_interval.tick() => broker.check_health().await?,
                    _ = lan_interval.tick(), if self.lan.is_some() => {
                        let status = lan::address().await;
                        if lan::changed(self.lan.unwrap(), &status) {
                            return Err(format!("LAN IPv4 changed from {:?} to {status:?}; MQTT requires restart", self.lan));
                        }
                        if let Err(error) = status {
                            eprintln!("[-] LAN IPv4 check failed: {error}");
                        }
                    }
                }
            }
        }
        #[cfg(not(feature = "mqtt"))]
        {
            let _ = devices;
            std::future::pending().await
        }
    }
}

#[cfg(test)]
mod tests;
