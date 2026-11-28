// Copyright 2026 Lazuli Kao / openwrt-fttr contributors
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod command;
pub mod netifd;
pub mod network;
#[cfg(all(
    feature = "npu",
    any(
        feature = "platform-h3c-hm2004-du",
        feature = "platform-fiberhome-an4475-g2"
    )
))]
pub mod npu;
pub mod onu_lock;
pub mod packet;
pub mod service;
pub(crate) mod wake;

pub use netifd::NetifdClient;
pub use service::ServiceClient;
