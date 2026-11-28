use super::command::{framed_script, parse_reply, CommandResult, CommandStatus};
use crate::mqtt::session::{self, CommandSession};
use rumqttc::{Event, Packet};
use std::{io, net::SocketAddr, time::Duration};
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicationState {
    NotEnqueued,
    Enqueued,
    FlushConfirmed,
}

impl PublicationState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotEnqueued => "not_enqueued",
            Self::Enqueued => "enqueued",
            Self::FlushConfirmed => "flush_confirmed",
        }
    }
}

pub(super) struct Request<'a> {
    pub target: String,
    pub script: &'a str,
    pub id: String,
    pub timeout: Duration,
}

pub(super) struct TransportError {
    pub error: io::Error,
    pub publication: PublicationState,
}

/// The only execution implementation, shared by direct and identity-guarded
/// callers. The synchronous enqueue callback must not wait or change the target.
pub(super) async fn run(
    address: SocketAddr,
    request: Request<'_>,
    enqueue: impl FnOnce(&mut CommandSession, &str, &[u8]) -> io::Result<()>,
) -> io::Result<CommandResult> {
    run_tracked(address, request, enqueue)
        .await
        .map(|(result, _)| result)
        .map_err(|failure| failure.error)
}

pub(super) async fn run_tracked(
    address: SocketAddr,
    request: Request<'_>,
    enqueue: impl FnOnce(&mut CommandSession, &str, &[u8]) -> io::Result<()>,
) -> Result<(CommandResult, PublicationState), TransportError> {
    let not_enqueued = |error| TransportError {
        error,
        publication: PublicationState::NotEnqueued,
    };
    // One end-to-end deadline, including connection, SUBACK and socket flush.
    // An expired deadline never means a remote shell was cancelled.
    let deadline = Instant::now() + request.timeout;
    let topic = format!("/fttr/subdev/exec/{}", request.target);
    let reply_topic = format!("{topic}/resp");
    let mut session = CommandSession::connect(
        address,
        &format!("fttr-exec-{}", request.id),
        deadline.min(Instant::now() + Duration::from_secs(5)),
    )
    .await
    .map_err(|error| not_enqueued(io::Error::other(error)))?;
    session
        .subscribe(&reply_topic, deadline)
        .await
        .map_err(|error| {
            let kind = if error.contains("SUBACK") {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::Other
            };
            not_enqueued(io::Error::new(kind, error))
        })?;
    enqueue(
        &mut session,
        &topic,
        framed_script(request.script, &request.id).as_bytes(),
    )
    .map_err(not_enqueued)?;
    session
        .flushed(deadline)
        .await
        .map_err(|error| TransportError {
            error: io::Error::other(error),
            publication: PublicationState::Enqueued,
        })?;
    let publication = PublicationState::FlushConfirmed;
    while Instant::now() < deadline {
        match session.next(deadline).await {
            Ok(Event::Incoming(Packet::Publish(message)))
                if !message.retain && message.topic == reply_topic =>
            {
                if let Some((output, code, output_truncated)) =
                    parse_reply(&message.payload, &request.id)
                {
                    return Ok((
                        CommandResult {
                            target: request.target,
                            status: CommandStatus::Executed,
                            request_id: Some(request.id),
                            exit_code: Some(code),
                            output: Some(output),
                            output_truncated,
                            error: None,
                        },
                        publication,
                    ));
                }
            }
            Err(_) if Instant::now() >= deadline => break,
            Err(error) => {
                return Err(TransportError {
                    error: io::Error::other(error),
                    publication,
                })
            }
            _ => (),
        }
    }
    Ok((
        CommandResult {
            target: request.target,
            status: CommandStatus::Timeout,
            request_id: Some(request.id),
            exit_code: None,
            output: None,
            output_truncated: false,
            error: Some(
                "no matching device response before timeout; execution outcome is unknown".into(),
            ),
        },
        publication,
    ))
}

pub(super) fn enqueue(session: &mut CommandSession, topic: &str, payload: &[u8]) -> io::Result<()> {
    session
        .enqueue_publish(topic, payload)
        .map_err(io::Error::other)
}

pub(super) fn blocking<T>(
    future: impl std::future::Future<Output = io::Result<T>>,
) -> io::Result<T> {
    session::blocking(async { Ok(future.await) }).map_err(io::Error::other)?
}

#[cfg(test)]
mod tests;
