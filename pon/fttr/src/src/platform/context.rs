//! A resolved platform is passed to workers; operations use its selected backend.
use super::{
    adapter::PlatformId,
    backend::{self, PlatformBackend},
    resources::Resources,
    runtime::SelectionError,
};
use crate::pon_engine::{
    omci::{session::OmciTransport, wire::BaselineFrame},
    ploam::PloamFrame,
};
use std::io;

#[derive(Clone, Copy, Debug)]
pub struct PlatformContext {
    backend: &'static dyn PlatformBackend,
}

pub trait ManagementLease {
    fn set_business_grant_floor(&mut self, bytes: u16) -> io::Result<()>;
    fn set_management_grant_floor(&mut self, bytes: u16) -> io::Result<()>;
    fn disarm(&mut self) -> io::Result<()>;
}

/// Opaque reversible platform state. Dropping it restores the previous mapping.
pub trait ChannelGuard {}

pub struct Transport(Box<dyn OmciTransport>);
impl OmciTransport for Transport {
    fn send(&mut self, frame: &BaselineFrame) -> io::Result<()> {
        self.0.send(frame)
    }
    fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
        self.0.receive()
    }
    fn set_deadline(&mut self, deadline: Option<std::time::Instant>) {
        self.0.set_deadline(deadline);
    }
}

impl PlatformContext {
    #[cfg(test)]
    pub(crate) fn test_context() -> Self {
        Self::from_backend(&tests::TEST_BACKEND).expect("valid test platform resources")
    }

    pub fn resolve(id: PlatformId) -> Result<Self, SelectionError> {
        Self::from_backend(backend::resolve(id)?)
    }

    fn from_backend(backend: &'static dyn PlatformBackend) -> Result<Self, SelectionError> {
        backend
            .resources()
            .validate()
            .map_err(|reason| SelectionError::InvalidResources(backend.descriptor().id, reason))?;
        Ok(Self { backend })
    }
    pub fn detect() -> Result<Self, SelectionError> {
        Self::resolve(super::runtime::detect()?)
    }
    pub fn id(self) -> PlatformId {
        self.backend.descriptor().id
    }
    pub fn resources(self) -> &'static Resources {
        self.backend.resources()
    }
    pub fn all_allocations(self) -> &'static [super::resources::OnuResources] {
        self.backend.all_allocations()
    }
    pub fn info(self) -> fttr_api::PlatformInfoReply {
        let descriptor = self.backend.descriptor();
        fttr_api::PlatformInfoReply {
            schema_version: 1,
            selection: fttr_api::PlatformSelection::Selected,
            platform_id: descriptor.id.as_str().to_owned(),
            vendor: descriptor.vendor.to_owned(),
            model: descriptor.model.to_owned(),
            onus: self
                .resources()
                .onu_ids
                .iter()
                .map(|&onu_id| fttr_api::PlatformOnuResource {
                    onu_id,
                    interface: self.resources().interface(onu_id).map(str::to_owned),
                })
                .collect(),
            operations: self.backend.operations(),
        }
    }
    pub(crate) fn backend(self) -> &'static dyn PlatformBackend {
        self.backend
    }

    pub fn transport(self, id: u8) -> io::Result<Transport> {
        self.resources().onu(id)?;
        Ok(Transport(self.backend.transport(id)?))
    }
    pub fn management_lease(self, id: u8) -> io::Result<Box<dyn ManagementLease>> {
        self.resources().onu(id)?;
        self.backend.management_lease(id)
    }
    pub fn channel(self, id: u8, channel: u16) -> io::Result<Box<dyn ChannelGuard>> {
        self.resources().onu(id)?;
        self.backend.channel(id, channel)
    }
    pub fn exchange_channel(self, id: u8, channel: u16) -> io::Result<u16> {
        self.resources().onu(id)?;
        self.backend.exchange_channel(id, channel)
    }
    pub fn send_ploam(self, frame: &PloamFrame) -> io::Result<()> {
        self.backend.send_ploam(frame)
    }
}

#[cfg(test)]
mod tests;
