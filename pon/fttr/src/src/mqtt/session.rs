//! One owned, fail-terminal command connection. Never poll rumqttc again after
//! an error: its normal reconnect policy could otherwise replay queued work.
use rumqttc::{
    AsyncClient, Event, EventLoop, MqttOptions, Outgoing, Packet, QoS, SubscribeReasonCode,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;

pub const MAX_REMAINING_BYTES: usize = crate::mqtt_limits::MAX_PACKET_BYTES - 5;

pub fn options(address: SocketAddr, id: &str) -> MqttOptions {
    let mut options = MqttOptions::new(id, address.ip().to_string(), address.port());
    options
        .set_clean_session(true)
        .set_keep_alive(Duration::from_secs(30))
        .set_max_packet_size(MAX_REMAINING_BYTES, crate::mqtt_limits::MAX_PACKET_BYTES)
        .set_inflight(16);
    options
}

struct Connection {
    client: AsyncClient,
    events: EventLoop,
    enqueued: bool,
}

pub struct CommandSession {
    connection: Option<Connection>,
}

impl CommandSession {
    pub async fn connect(address: SocketAddr, id: &str, deadline: Instant) -> Result<Self, String> {
        let (client, events) = AsyncClient::new(options(address, id), 1);
        let mut session = Self {
            connection: Some(Connection {
                client,
                events,
                enqueued: false,
            }),
        };
        match session.next(deadline).await? {
            Event::Incoming(Packet::ConnAck(_)) => Ok(session),
            _ => session.fail("MQTT connection missing CONNACK"),
        }
    }

    fn fail<T>(&mut self, error: impl Into<String>) -> Result<T, String> {
        // Drop socket, client and every pending request together, including on
        // timeout/cancellation of our caller. There is no detached poll task.
        self.connection = None;
        Err(error.into())
    }

    pub async fn next(&mut self, deadline: Instant) -> Result<Event, String> {
        // Taking ownership also makes cancellation of THIS future terminal,
        // even if its caller retains the CommandSession value.
        let Some(mut connection) = self.connection.take() else {
            return Err("MQTT command session is terminal".into());
        };
        if Instant::now() >= deadline {
            return self.fail("MQTT command deadline exceeded");
        }
        match tokio::time::timeout_at(deadline, connection.events.poll()).await {
            Ok(Ok(event)) => { self.connection = Some(connection); Ok(event) },
            Ok(Err(error)) => self.fail(format!("MQTT command connection failed: {error}; outcome may be unknown; command will not be retried")),
            Err(_) => self.fail("MQTT command deadline exceeded; outcome may be unknown; command will not be retried"),
        }
    }

    pub async fn subscribe(&mut self, topic: &str, deadline: Instant) -> Result<(), String> {
        let Some(connection) = &self.connection else {
            return Err("MQTT command session is terminal".into());
        };
        if let Err(error) = connection.client.try_subscribe(topic, QoS::AtMostOnce) {
            return self.fail(format!("MQTT subscription enqueue failed: {error}"));
        }
        let mut expected = None;
        loop {
            match self.next(deadline).await? {
                Event::Outgoing(Outgoing::Subscribe(id)) => expected = Some(id),
                Event::Incoming(Packet::SubAck(ack)) => {
                    if expected != Some(ack.pkid)
                        || ack.return_codes != [SubscribeReasonCode::Success(QoS::AtMostOnce)]
                    {
                        return self.fail("MQTT subscription rejected or SUBACK did not match");
                    }
                    return Ok(());
                }
                _ => (),
            }
        }
    }

    /// Nonblocking so a caller can recheck identity and enqueue atomically
    /// under its synchronous registry lock, releasing that lock before await.
    pub fn enqueue_publish(&mut self, topic: &str, payload: &[u8]) -> Result<(), String> {
        if let Err(error) = crate::mqtt_limits::publish_size(topic.len(), payload.len()) {
            return self.fail(error.to_string());
        }
        let Some(connection) = self.connection.as_mut() else {
            return Err("MQTT command session is terminal".into());
        };
        if connection.enqueued {
            return self.fail("MQTT command session permits only one publication");
        }
        if let Err(error) = connection
            .client
            .try_publish(topic, QoS::AtMostOnce, false, payload)
        {
            return self.fail(format!("MQTT publication enqueue failed: {error}"));
        }
        connection.enqueued = true;
        Ok(())
    }

    /// rumqttc 0.25.1 select() writes and flushes BEFORE yielding this event.
    /// On this dedicated session its one QoS0 publication is unambiguous.
    pub async fn flushed(&mut self, deadline: Instant) -> Result<(), String> {
        if !self.connection.as_ref().is_some_and(|c| c.enqueued) {
            return self.fail("MQTT publication was not enqueued");
        }
        loop {
            if matches!(
                self.next(deadline).await?,
                Event::Outgoing(Outgoing::Publish(0))
            ) {
                return Ok(());
            }
        }
    }
}

/// Synchronous device adapter boundary, sharing the application executor.
pub fn blocking<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    crate::runtime::block_on(future)
}

#[cfg(test)]
mod tests;
