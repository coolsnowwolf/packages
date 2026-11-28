use super::{
    attributes::{AttributeBlock, AttributeCodec, AttributeContext, AttributeValue},
    error::invalid,
    messages::{check_result, Action, GetResponse, RequestBody},
    mib::{MibAssembler, MibRecord, MibReport},
    schema::MeRegistry,
    tables::TableAssembler,
    wire::BaselineFrame,
};
use std::{
    collections::BTreeSet,
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

/// Project transport boundary; returns only validated, ONU-isolated protocol frames.
pub trait OmciTransport: Send {
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()>;
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>>;
    fn set_deadline(&mut self, _deadline: Option<Instant>) {}
}

#[cfg(test)]
mod allocation_tests {
    use super::*;

    #[test]
    fn allocator_wrap_skips_in_flight_transactions_and_can_reuse_released_tci() {
        let mut pool = TciPool {
            next: 0x7fff,
            in_use: BTreeSet::from([1, 2, 0x7fff]),
        };
        assert_eq!(pool.reserve().unwrap(), 3);
        pool.in_use.remove(&1);
        pool.next = 0x7fff;
        assert_eq!(pool.reserve().unwrap(), 1);
        pool.in_use = (1..=0x7fff).collect();
        assert_eq!(
            pool.reserve().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn request_drop_releases_its_tci() {
        let request = request_frame(0x49, 256, 0, [0; 32]).unwrap();
        let tci = request.transaction_id;
        assert!(tci_pool().lock().unwrap().in_use.contains(&tci));
        drop(request);
        // Other tests may allocate concurrently, but the round-robin allocator
        // will not wrap thousands of requests within this assertion.
        assert!(!tci_pool().lock().unwrap().in_use.contains(&tci));
    }
}

/// Failure classification for strictly single-shot OMCI operations.
#[derive(Debug)]
pub enum SingleShotError {
    NotSent(io::Error),
    Rejected(io::Error),
    UncertainAfterSend(io::Error),
}

impl SingleShotError {
    pub fn into_io_error(self) -> io::Error {
        match self {
            Self::NotSent(e) | Self::Rejected(e) | Self::UncertainAfterSend(e) => e,
        }
    }
}

/// OMCI-001/002/003/008 message semantics; retries/guards are project orchestration.
/// No socket, FPGA register or device topology is owned by this object.
pub struct OmciSession {
    transport: Box<dyn OmciTransport>,
    registration: Option<Box<dyn Fn() -> bool + Send>>,
    cancellation: Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
}

/// An upload snapshot owned by one serialized ONU worker. It must be discarded
/// if another MIB upload is started or the registration changes.
#[cfg(feature = "platform-fiberhome-an4475-g2")]
pub(crate) struct MibUpload {
    assembler: Option<MibAssembler>,
    next: u16,
    count: u16,
    pending: Option<MibTransaction>,
}

#[cfg(feature = "platform-fiberhome-an4475-g2")]
struct MibTransaction {
    request: RequestFrame,
    sent_at: Option<Instant>,
    attempts: u8,
}

struct TciPool {
    next: u16,
    in_use: BTreeSet<u16>,
}

impl TciPool {
    fn reserve(&mut self) -> io::Result<u16> {
        for _ in 0..0x7fff {
            let tci = self.next;
            self.next = (tci.wrapping_add(1) & 0x7fff).max(1);
            if self.in_use.insert(tci) {
                return Ok(tci);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "OMCI TCI space exhausted",
        ))
    }
}

fn tci_pool() -> &'static Mutex<TciPool> {
    static POOL: OnceLock<Mutex<TciPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        Mutex::new(TciPool {
            next: 1,
            in_use: BTreeSet::new(),
        })
    })
}

/// Keep an in-flight TCI reserved even while other sessions wrap the allocator.
struct RequestFrame(BaselineFrame);

impl std::ops::Deref for RequestFrame {
    type Target = BaselineFrame;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for RequestFrame {
    fn drop(&mut self) {
        tci_pool()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_use
            .remove(&self.transaction_id);
    }
}

fn request_frame(
    message_type: u8,
    class_id: u16,
    entity_id: u16,
    payload: [u8; 32],
) -> io::Result<RequestFrame> {
    let tci = tci_pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reserve()?;
    Ok(RequestFrame(BaselineFrame::new(
        tci,
        message_type,
        class_id,
        entity_id,
        payload,
    )))
}

impl OmciSession {
    pub fn new(transport: impl OmciTransport + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            registration: None,
            cancellation: None,
            deadline: None,
        }
    }
    pub fn guard_registration(&mut self, guard: impl Fn() -> bool + Send + 'static) {
        self.registration = Some(Box::new(guard));
    }
    pub fn cancel_when(&mut self, token: Arc<AtomicBool>) {
        self.cancellation = Some(token);
    }
    pub fn finish_before(&mut self, deadline: Instant) {
        self.deadline = Some(deadline);
        self.transport.set_deadline(self.deadline);
    }
    /// Bound an optional operation without extending or poisoning the caller's deadline.
    pub fn with_timeout<T>(
        &mut self,
        timeout: Duration,
        operation: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<T> {
        let previous = self.deadline;
        let deadline = Instant::now() + timeout;
        self.deadline = Some(previous.map_or(deadline, |outer| outer.min(deadline)));
        self.transport.set_deadline(self.deadline);
        let result = operation(self);
        self.deadline = previous;
        self.transport.set_deadline(previous);
        result
    }
    fn check_active(&self) -> io::Result<()> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "OMCI diagnostic deadline exceeded",
            ));
        }
        if self
            .cancellation
            .as_ref()
            .is_some_and(|t| t.load(Ordering::Acquire))
            || self.registration.as_ref().is_some_and(|g| !g())
        {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "ONU registration generation no longer active",
            ));
        }
        Ok(())
    }
    fn transact_attempts(
        &mut self,
        message_type: u8,
        class_id: u16,
        entity_id: u16,
        payload: [u8; 32],
        max_attempts: u8,
    ) -> Result<BaselineFrame, SingleShotError> {
        let request = request_frame(message_type, class_id, entity_id, payload)
            .map_err(SingleShotError::NotSent)?;
        let mut sent = false;
        let fail = |sent: bool, error: io::Error| {
            if sent {
                SingleShotError::UncertainAfterSend(error)
            } else {
                SingleShotError::NotSent(error)
            }
        };
        for _ in 0..max_attempts {
            self.check_active().map_err(|e| fail(sent, e))?;
            // Mark attempted before calling transport.send so ambiguous lower-layer
            // errors (such as a post-submit completion timeout) are never misreported
            // as "not sent".
            sent = true;
            self.transport
                .send(&request)
                .map_err(SingleShotError::UncertainAfterSend)?;
            // Building a MIB snapshot can take much longer than an ordinary
            // attribute operation. Keep the same TCI while awaiting that reply;
            // rapidly starting another upload can restart the ONU's snapshot.
            let timeout = if message_type == Action::MibUpload.request_type() {
                Duration::from_secs(20)
            } else {
                Duration::from_secs(2)
            };
            let end = Instant::now() + timeout;
            while Instant::now() < end {
                self.check_active()
                    .map_err(SingleShotError::UncertainAfterSend)?;
                match self
                    .transport
                    .receive()
                    .map_err(SingleShotError::UncertainAfterSend)?
                {
                    Some(frame) if frame.answers(&request) => {
                        std::thread::sleep(Duration::from_millis(25));
                        return Ok(frame);
                    }
                    Some(_) => std::thread::sleep(Duration::from_millis(2)),
                    None => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        }
        Err(SingleShotError::UncertainAfterSend(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "OMCI TCI={:04x} class={class_id} entity={entity_id:04x}",
                request.transaction_id
            ),
        )))
    }
    /// Raw vendor requests remain explicit at the device adapter boundary.
    pub fn transact(
        &mut self,
        message_type: u8,
        class_id: u16,
        entity_id: u16,
        payload: [u8; 32],
    ) -> io::Result<BaselineFrame> {
        self.transact_attempts(message_type, class_id, entity_id, payload, 3)
            .map_err(SingleShotError::into_io_error)
    }
    /// Sends at most one OMCI request frame without retrying on timeout.
    pub fn transact_once(
        &mut self,
        message_type: u8,
        class_id: u16,
        entity_id: u16,
        payload: [u8; 32],
    ) -> Result<BaselineFrame, SingleShotError> {
        self.transact_attempts(message_type, class_id, entity_id, payload, 1)
    }
    fn request(
        &mut self,
        action: Action,
        class_id: u16,
        entity_id: u16,
        body: RequestBody<'_>,
    ) -> io::Result<BaselineFrame> {
        self.transact(action.request_type(), class_id, entity_id, body.encode()?)
    }
    pub fn get(&mut self, class_id: u16, entity_id: u16, mask: u16) -> io::Result<BaselineFrame> {
        let frame = self.request(Action::Get, class_id, entity_id, RequestBody::Get { mask })?;
        GetResponse::decode(&frame, mask)?;
        Ok(frame)
    }
    pub fn get_attributes(
        &mut self,
        class_id: u16,
        entity_id: u16,
        mask: u16,
    ) -> io::Result<AttributeBlock> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported)
            .ok_or_else(|| invalid("no complete ME layout"))?;
        let frame = self.get(class_id, entity_id, mask)?;
        let body = GetResponse::decode(&frame, mask)?;
        let block =
            AttributeCodec::decode(def, body.mask, body.data, AttributeContext::GetResponse);
        if block.undecoded_mask != 0 {
            return Err(invalid("GET attribute block cannot be fully decoded"));
        }
        Ok(block)
    }
    pub fn set_attributes(
        &mut self,
        class_id: u16,
        entity_id: u16,
        values: &[(u8, AttributeValue)],
    ) -> io::Result<()> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported)
            .ok_or_else(|| invalid("no complete ME layout"))?;
        let (mask, data) = AttributeCodec::encode(def, values, AttributeContext::SetRequest)?;
        self.set(class_id, entity_id, mask, &data)
    }
    pub fn create_attributes(
        &mut self,
        class_id: u16,
        entity_id: u16,
        values: &[(u8, AttributeValue)],
    ) -> io::Result<()> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported)
            .ok_or_else(|| invalid("no complete ME layout"))?;
        let (_, data) = AttributeCodec::encode(def, values, AttributeContext::CreateRequest)?;
        self.create(class_id, entity_id, &data)
    }
    pub fn read_attributes(
        &mut self,
        class_id: u16,
        entity_id: u16,
        indices: &[u8],
    ) -> io::Result<AttributeBlock> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported && d.actions.contains(&9))
            .ok_or_else(|| invalid("no complete readable ME layout"))?;
        self.get_attributes(
            class_id,
            entity_id,
            AttributeCodec::read_mask(def, indices)?,
        )
    }
    /// Compare exact encoded octets, including fixed-width string padding. Never log values.
    pub fn verify_attributes(
        &mut self,
        class_id: u16,
        entity_id: u16,
        values: &[(u8, AttributeValue)],
    ) -> io::Result<()> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported)
            .ok_or_else(|| invalid("no complete ME layout"))?;
        let indices: Vec<_> = values.iter().map(|(index, _)| *index).collect();
        AttributeCodec::read_mask(def, &indices)?;
        let expected: Vec<_> = values
            .iter()
            .map(|(index, value)| {
                AttributeCodec::encode_value(def.attribute(*index).unwrap(), value)
                    .map(|bytes| (*index, super::wire::hex(&bytes)))
            })
            .collect::<io::Result<_>>()?;
        let block = self.read_attributes(class_id, entity_id, &indices)?;
        for (index, expected) in expected {
            let attr = def.attribute(index).unwrap();
            if !block
                .attributes
                .iter()
                .any(|a| a.definition.index == index && a.raw_hex == expected)
            {
                return Err(invalid(format!(
                    "readback mismatch class={class_id} instance={entity_id:04x} attribute={}",
                    attr.name
                )));
            }
        }
        Ok(())
    }
    pub fn set(&mut self, class_id: u16, entity_id: u16, mask: u16, data: &[u8]) -> io::Result<()> {
        let frame = self.request(
            Action::Set,
            class_id,
            entity_id,
            RequestBody::Set { mask, data },
        )?;
        check_result(&frame)
    }
    pub fn set_once(
        &mut self,
        class_id: u16,
        entity_id: u16,
        mask: u16,
        data: &[u8],
    ) -> Result<(), SingleShotError> {
        let payload = RequestBody::Set { mask, data }
            .encode()
            .map_err(SingleShotError::NotSent)?;
        let frame = self.transact_once(Action::Set.request_type(), class_id, entity_id, payload)?;
        check_result(&frame).map_err(SingleShotError::Rejected)
    }
    pub fn set_attributes_once(
        &mut self,
        class_id: u16,
        entity_id: u16,
        values: &[(u8, AttributeValue)],
    ) -> Result<(), SingleShotError> {
        let def = MeRegistry::get(class_id)
            .filter(|d| d.layout_supported)
            .ok_or_else(|| SingleShotError::NotSent(invalid("no complete ME layout")))?;
        let (mask, data) = AttributeCodec::encode(def, values, AttributeContext::SetRequest)
            .map_err(SingleShotError::NotSent)?;
        self.set_once(class_id, entity_id, mask, &data)
    }
    pub fn create(&mut self, class_id: u16, entity_id: u16, data: &[u8]) -> io::Result<()> {
        let frame = self.request(
            Action::Create,
            class_id,
            entity_id,
            RequestBody::Create { data },
        )?;
        check_result(&frame)
    }
    pub fn delete(&mut self, class_id: u16, entity_id: u16) -> io::Result<()> {
        let frame = self.request(Action::Delete, class_id, entity_id, RequestBody::Empty)?;
        check_result(&frame)
    }
    pub fn get_table(&mut self, class_id: u16, entity_id: u16, mask: u16) -> io::Result<Vec<u8>> {
        if mask.count_ones() != 1 {
            return Err(invalid("GET NEXT requires one table attribute"));
        }
        let frame = self.get(class_id, entity_id, mask)?;
        let length = u32::from_be_bytes(frame.payload[3..7].try_into().unwrap()) as usize;
        let mut table = TableAssembler::new(length)?;
        for sequence in 0..table.segments() as u16 {
            let frame = self.request(
                Action::GetNext,
                class_id,
                entity_id,
                RequestBody::GetNext { mask, sequence },
            )?;
            let body = GetResponse::decode(&frame, mask)?;
            table.push(sequence, body.data)?;
        }
        table.finish()
    }
    pub fn upload_mib(&mut self) -> io::Result<MibReport> {
        self.upload_mib_with_progress(|_, _| Ok(()))
    }

    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    pub(crate) fn begin_mib_upload(&mut self) -> MibUpload {
        MibUpload {
            assembler: None,
            next: 0,
            count: 0,
            pending: None,
        }
    }

    /// Retain the exact request and retry state when an automatic poll slice ends.
    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    fn resume_mib_transaction(
        &mut self,
        pending: &mut MibTransaction,
    ) -> io::Result<BaselineFrame> {
        let timeout = if pending.request.message_type == Action::MibUpload.request_type() {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(2)
        };
        loop {
            self.check_active()?;
            if pending.sent_at.is_some() {
                if let Some(frame) = self.transport.receive()? {
                    if frame.answers(&pending.request) {
                        std::thread::sleep(Duration::from_millis(25));
                        return Ok(frame);
                    }
                }
            }
            if pending.sent_at.is_none_or(|sent| sent.elapsed() >= timeout) {
                if pending.attempts == 3 {
                    // Distinguish exhausted retries from the resumable slice deadline.
                    return Err(io::Error::other("MIB transaction exhausted retries"));
                }
                self.transport.send(&pending.request)?;
                pending.sent_at = Some(Instant::now());
                pending.attempts += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Fetch exactly one record; a failed request never advances the cursor.
    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    pub(crate) fn next_mib_record(&mut self, upload: &mut MibUpload) -> io::Result<bool> {
        if upload.assembler.is_some() && upload.next >= upload.count {
            return Ok(true);
        }
        let sequence = upload.next;
        let starting = upload.assembler.is_none();
        if upload.pending.is_none() {
            let (action, body) = if starting {
                (Action::MibUpload, RequestBody::Empty)
            } else {
                (Action::MibUploadNext, RequestBody::UploadNext { sequence })
            };
            upload.pending = Some(MibTransaction {
                request: request_frame(action.request_type(), 2, 0, body.encode()?)?,
                sent_at: None,
                attempts: 0,
            });
        }
        let frame = self.resume_mib_transaction(upload.pending.as_mut().unwrap())?;
        upload.pending = None;
        if starting {
            let count = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
            if count == 0 || count > 2048 {
                return Err(invalid(format!(
                    "invalid/beyond supported MIB upload count: {count}"
                )));
            }
            upload.count = count;
            upload.assembler = Some(MibAssembler::new(count));
            return Ok(false);
        }
        let record = MibRecord::decode(sequence, &frame.payload).map_err(|error| {
            invalid(format!("MIB sequence={sequence}/{}: {error}", upload.count))
        })?;
        upload.assembler.as_mut().unwrap().push(record);
        upload.next += 1;
        Ok(upload.next == upload.count)
    }

    #[cfg(feature = "platform-fiberhome-an4475-g2")]
    pub(crate) fn finish_mib_upload(upload: MibUpload) -> MibReport {
        upload.assembler.expect("completed MIB upload").finish()
    }
    pub fn upload_mib_with_progress(
        &mut self,
        mut progress: impl FnMut(usize, usize) -> io::Result<()>,
    ) -> io::Result<MibReport> {
        let frame = self.request(Action::MibUpload, 2, 0, RequestBody::Empty)?;
        let count = u16::from_be_bytes([frame.payload[0], frame.payload[1]]);
        if count == 0 || count > 2048 {
            return Err(invalid(format!(
                "invalid/beyond supported MIB upload count: {count} (raw {:02x}{:02x})",
                frame.payload[0], frame.payload[1]
            )));
        }
        let mut mib = MibAssembler::new(count);
        progress(0, usize::from(count))?;
        for sequence in 0..count {
            let frame = self.request(
                Action::MibUploadNext,
                2,
                0,
                RequestBody::UploadNext { sequence },
            )?;
            mib.push(MibRecord::decode(sequence, &frame.payload)?);
            progress(usize::from(sequence) + 1, usize::from(count))?;
        }
        Ok(mib.finish())
    }
}
