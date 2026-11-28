use super::{command, command_transport};
use crate::mqtt::session::CommandSession;
use crate::mqtt_identity::{Registry, Selection};
pub use fttr_api::HlOperation as SelectedResult;
use std::{
    io,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn enqueue_guarded(
    session: &mut CommandSession,
    registry: &Mutex<Registry>,
    selection: &Selection,
    target: &str,
    topic: &str,
    payload: &[u8],
    now: u64,
    transaction: &command::Transaction,
) -> io::Result<()> {
    let state = registry
        .lock()
        .map_err(|_| io::Error::other("MQTT identity state unavailable"))?;
    let snapshot = state
        .resolve(selection, now)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    if snapshot.mqtt_mac != target || topic != format!("/fttr/subdev/exec/{target}") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MQTT target changed before publication",
        ));
    }
    // Resolve and nonblocking enqueue are atomic against rebinding. No await
    // occurs with this lock held; a full queue is a terminal local error.
    transaction.enqueue_authorized(session, topic, payload)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn execute(
    selection: &Selection,
    script: &str,
    timeout: Duration,
) -> io::Result<SelectedResult> {
    crate::runtime::block_on(execute_async(selection, script, timeout))
}

pub async fn execute_async(
    selection: &Selection,
    script: &str,
    timeout: Duration,
) -> io::Result<SelectedResult> {
    if script.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command must not be empty",
        ));
    }
    let target = crate::mqtt_identity::shared()
        .lock()
        .map_err(|_| io::Error::other("MQTT identity state unavailable"))?
        .resolve(selection, now())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let transaction = command::Transaction::acquire(&target.mqtt_mac)?;
    // Opaque external scripts have no idempotent replay contract. Persist only
    // identity and intent metadata; never persist the script or its secrets.
    let path = crate::provisioning::runtime_store::Record::External.path(&selection.optical_sn);
    let intent = serde_json::json!({"schema_version":1,"pending":true,
        "binding_generation":selection.binding_generation,"onu_id":selection.onu_id,
        "created_at":now()});
    let save = |value: &serde_json::Value| -> io::Result<()> {
        crate::provisioning::storage::write_atomic(
            &path,
            &serde_json::to_vec(value).map_err(io::Error::other)?,
            true,
        )
    };
    save(&intent)?;
    let result = execute_transaction_async(&transaction, selection, script, timeout).await?;
    if result.binding_current
        && result.result.status == command::CommandStatus::Executed
        && result.result.exit_code == Some(0)
    {
        crate::provisioning::storage::remove_registry(&path).map_err(io::Error::other)?;
    }
    Ok(result)
}

pub(super) fn ensure_no_pending(serial: &str) -> io::Result<()> {
    for path in [
        crate::provisioning::runtime_store::Record::External.path(serial),
        super::provisioning::ownership_path(serial),
    ] {
        match crate::provisioning::storage::read(path) {
            Ok(bytes) => {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                if value["pending"].as_bool() != Some(false) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "unresolved command intent; transaction-aware recovery required",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) async fn execute_transaction_async(
    transaction: &command::Transaction,
    selection: &Selection,
    script: &str,
    timeout: Duration,
) -> io::Result<SelectedResult> {
    execute_at_async(
        "127.0.0.1:1883".parse().unwrap(),
        crate::mqtt_identity::shared(),
        selection,
        command::request_id(),
        script,
        timeout,
        now,
        Some(transaction),
    )
    .await
    .map(|(result, _)| result)
    .map_err(|failure| failure.error)
}

pub(super) async fn execute_request_async(
    transaction: &command::Transaction,
    selection: &Selection,
    request_id: String,
    script: &str,
    timeout: Duration,
) -> Result<
    (SelectedResult, command_transport::PublicationState),
    (io::Error, command_transport::PublicationState),
> {
    execute_at_async(
        "127.0.0.1:1883".parse().unwrap(),
        crate::mqtt_identity::shared(),
        selection,
        request_id,
        script,
        timeout,
        now,
        Some(transaction),
    )
    .await
    .map_err(|failure| (failure.error, failure.publication))
}

#[cfg(test)]
fn execute_at(
    address: std::net::SocketAddr,
    registry: &Mutex<Registry>,
    selection: &Selection,
    script: &str,
    timeout: Duration,
    clock: impl Fn() -> u64,
) -> io::Result<SelectedResult> {
    crate::runtime::block_on(execute_at_async(
        address,
        registry,
        selection,
        command::request_id(),
        script,
        timeout,
        clock,
        None,
    ))
    .map(|(result, _)| result)
    .map_err(|failure| failure.error)
}

async fn execute_at_async(
    address: std::net::SocketAddr,
    registry: &Mutex<Registry>,
    selection: &Selection,
    request_id: String,
    script: &str,
    timeout: Duration,
    clock: impl Fn() -> u64,
    transaction: Option<&command::Transaction>,
) -> Result<(SelectedResult, command_transport::PublicationState), command_transport::TransportError>
{
    let not_enqueued = |error| command_transport::TransportError {
        error,
        publication: command_transport::PublicationState::NotEnqueued,
    };
    let target = registry
        .lock()
        .map_err(|_| not_enqueued(io::Error::other("MQTT identity state unavailable")))?
        .resolve(selection, clock())
        .map_err(|message| not_enqueued(io::Error::new(io::ErrorKind::InvalidInput, message)))?;
    let owned_transaction;
    let transaction = match transaction {
        Some(transaction) => transaction,
        None => {
            owned_transaction =
                command::Transaction::acquire(&target.mqtt_mac).map_err(not_enqueued)?;
            &owned_transaction
        }
    };
    if transaction.target() != target.mqtt_mac || !transaction.matches_selection(selection) {
        return Err(not_enqueued(io::Error::new(
            io::ErrorKind::InvalidInput,
            "transaction target changed",
        )));
    }
    let _target_lock = command::lock_target(&target.mqtt_mac).map_err(not_enqueued)?;
    let (result, publication) = command_transport::run_tracked(
        address,
        command_transport::Request {
            target: target.mqtt_mac.clone(),
            script,
            id: request_id,
            timeout,
        },
        |session, topic, payload| {
            enqueue_guarded(
                session,
                registry,
                selection,
                &target.mqtt_mac,
                topic,
                payload,
                clock(),
                transaction,
            )
        },
    )
    .await?;
    let binding_current = registry
        .lock()
        .map_err(|_| command_transport::TransportError {
            error: io::Error::other("MQTT identity state unavailable"),
            publication,
        })?
        .resolve(selection, clock())
        .is_ok();
    Ok((
        SelectedResult {
            target,
            result,
            binding_current,
        },
        publication,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mqtt::test_peer::{self, read, write};
    use rumqttc::{Packet, QoS, SubAck, SubscribeReasonCode};
    use std::{collections::BTreeMap, io::Read, sync::Arc};

    #[test]
    fn external_and_native_pending_or_corrupt_intents_block_every_transaction() {
        let serial = "TEST00000017";
        let native = super::super::provisioning::ownership_path(serial);
        assert!(native.ends_with("/TEST00000017.json"));
        for path in [
            crate::provisioning::runtime_store::Record::External.path(serial),
            native,
        ] {
            for bytes in [b"{\"pending\":true}".as_slice(), b"{", b"{}"] {
                crate::provisioning::storage::write_atomic(&path, bytes, true).unwrap();
                assert!(ensure_no_pending(serial).is_err());
            }
            crate::provisioning::storage::write_atomic(&path, b"{\"pending\":false}", true)
                .unwrap();
            ensure_no_pending(serial).unwrap();
        }
    }

    #[test]
    fn command_binding_reply_reports_rebound_without_retargeting() {
        let registry = Arc::new(Mutex::new(Registry::new("daemon-b".into())));
        {
            let mut state = registry.lock().unwrap();
            state.observe("02AABBCCDDEF", 100);
            state.update_topology(
                BTreeMap::from([(3, "H3CT12345679".into())]),
                Ok(BTreeMap::from([(
                    "02AABBCCDDEF".into(),
                    "H3CT12345679".into(),
                )])),
                100,
            );
        }
        let selection = Selection {
            optical_sn: "H3CT12345679".into(),
            onu_id: 3,
            binding_generation: registry.lock().unwrap().snapshots(100)[0]
                .binding_generation
                .clone(),
        };
        let transaction = command::Transaction::acquire("02AABBCCDDEF").unwrap();
        let blocked = execute_at(
            "127.0.0.1:1".parse().unwrap(),
            &registry,
            &selection,
            "fixture",
            Duration::from_secs(1),
            || 100,
        )
        .unwrap_err();
        assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
        drop(transaction);
        let peer_registry = registry.clone();
        let (address, worker) = test_peer::spawn(move |mut stream| {
            let Packet::Subscribe(subscription) = read(&mut stream).unwrap() else {
                panic!("expected subscription")
            };
            write(
                &mut stream,
                Packet::SubAck(SubAck::new(
                    subscription.pkid,
                    vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
                )),
                false,
            );
            let Packet::Publish(published) = read(&mut stream).unwrap() else {
                panic!("expected publication")
            };
            assert_eq!(published.topic, "/fttr/subdev/exec/02AABBCCDDEF");
            let script = std::str::from_utf8(&published.payload).unwrap();
            let id = script
                .split("__FTTR_BEGIN_")
                .nth(1)
                .unwrap()
                .split("__")
                .next()
                .unwrap();
            peer_registry.lock().unwrap().update_topology(
                BTreeMap::new(),
                Ok(BTreeMap::new()),
                100,
            );
            write(
                &mut stream,
                Packet::Publish(rumqttc::Publish::new(
                    format!("{}/resp", published.topic),
                    QoS::AtMostOnce,
                    format!("__FTTR_BEGIN_{id}__\nfixture\n__FTTR_END_{id}__=7\n"),
                )),
                false,
            );
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let result = execute_at(
            address,
            &registry,
            &selection,
            "fixture",
            Duration::from_secs(1),
            || 100,
        )
        .unwrap();
        assert_eq!(result.target.mqtt_mac, "02AABBCCDDEF");
        assert_eq!(result.result.exit_code, Some(7));
        assert!(!result.binding_current);
        worker.join().unwrap();
    }

    #[test]
    fn mqtt_identity_changed_after_subscribe_prevents_any_publication() {
        let registry = Arc::new(Mutex::new(Registry::new("daemon-a".into())));
        {
            let mut state = registry.lock().unwrap();
            state.observe("02AABBCCDDEE", 100);
            state.update_topology(
                BTreeMap::from([(2, "H3CT12345678".into())]),
                Ok(BTreeMap::from([(
                    "02AABBCCDDEE".into(),
                    "H3CT12345678".into(),
                )])),
                100,
            );
        }
        let selection = Selection {
            optical_sn: "H3CT12345678".into(),
            onu_id: 2,
            binding_generation: registry.lock().unwrap().snapshots(100)[0]
                .binding_generation
                .clone(),
        };
        let peer_registry = registry.clone();
        let (address, worker) = test_peer::spawn(move |mut stream| {
            let Packet::Subscribe(subscription) = read(&mut stream).unwrap() else {
                panic!("expected subscription")
            };
            peer_registry.lock().unwrap().update_topology(
                BTreeMap::from([(2, "H3CT87654321".into())]),
                Ok(BTreeMap::new()),
                100,
            );
            write(
                &mut stream,
                Packet::SubAck(SubAck::new(
                    subscription.pkid,
                    vec![SubscribeReasonCode::Success(QoS::AtMostOnce)],
                )),
                true,
            );
            assert_eq!(stream.read(&mut [0]).unwrap(), 0); // No command publication.
        });
        let result = execute_at(
            address,
            &registry,
            &selection,
            "fixture",
            Duration::from_secs(1),
            || 100,
        );
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        worker.join().unwrap();
    }
}
