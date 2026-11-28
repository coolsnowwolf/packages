//! Standard G.988 ME 340 bootstrap. Association readback is not WAN/Inform proof.
use crate::pon_engine::omci::{
    attributes::AttributeValue, error::is_unknown_instance, wire::unhex, OmciSession,
};
use std::io;
fn ensure_auth_association_supported(username: &str) -> io::Result<()> {
    if username.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OMCI authentication association not verified",
        ))
    }
}
pub fn configure(
    session: &mut OmciSession,
    instance: u16,
    url: &str,
    vlan: u32,
    priority: u32,
) -> io::Result<()> {
    let tag = management_tag(vlan, priority)?;
    ensure_auth_association_supported(&crate::startup_config::get().acs_username)?;
    if !url.starts_with("http://")
        || url.len() > 374
        || !url.is_ascii()
        || url.bytes().any(|b| b.is_ascii_control())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid OMCI ACS URL",
        ));
    }
    let before = session.read_attributes(340, instance, &[1, 2, 3])?;
    let original: Vec<_> = before
        .attributes
        .iter()
        .map(|a| (a.definition.index, a.value.clone()))
        .collect();
    let admin = original[0].1.unsigned().unwrap();
    let pointer = original[1].1.unsigned().unwrap();
    let old_tag = original[2].1.unsigned().unwrap();
    if !(admin == 1 && pointer == 0xffff && old_tag == 0xffff) && !(admin <= 1 && pointer == 0x8001)
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "foreign OMCI ACS association",
        ));
    }
    match session.read_attributes(157, 0x200, &[1]) {
        Ok(block) => {
            let count = block.attributes[0].value.unsigned().unwrap() as usize;
            if count > 15 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid OMCI URL length",
                ));
            }
            let mut value = Vec::new();
            for n in 0..count {
                let part = session.read_attributes(157, 0x200, &[n as u8 + 2])?;
                value.extend(unhex(&part.attributes[0].raw_hex)?);
            }
            while value.last() == Some(&0) {
                value.pop();
            }
            if value != url.as_bytes() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "existing OMCI ACS URL differs",
                ));
            }
        }
        Err(e) if is_unknown_instance(&e) => {
            session.create_attributes(157, 0x200, &[])?;
            for (n, part) in url.as_bytes().chunks(25).enumerate() {
                let mut data = [0; 25];
                data[..part.len()].copy_from_slice(part);
                let values = [(n as u8 + 2, AttributeValue::octets(&data))];
                session.set_attributes(157, 0x200, &values)?;
                session.verify_attributes(157, 0x200, &values)?;
            }
            let count = url.len().div_ceil(25) as u8;
            let values = [(1, AttributeValue::number(count.into()))];
            session.set_attributes(157, 0x200, &values)?;
            session.verify_attributes(157, 0x200, &values)?;
        }
        Err(e) => return Err(e),
    }
    let address = [
        (1, AttributeValue::number(0)),
        (2, AttributeValue::number(0x200)),
    ];
    match session.read_attributes(137, 0x8001, &[1, 2]) {
        Ok(block) => {
            if block
                .attributes
                .iter()
                .zip(&address)
                .any(|(actual, (_, expected))| actual.value != *expected)
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "existing OMCI network address differs",
                ));
            }
        }
        Err(e) if is_unknown_instance(&e) => {
            session.create_attributes(137, 0x8001, &address)?;
            session.verify_attributes(137, 0x8001, &address)?;
        }
        Err(e) => return Err(e),
    }
    let tag = u16::from_be_bytes(tag) as u32;
    let association = [
        (2, AttributeValue::number(0x8001)),
        (3, AttributeValue::number(tag)),
    ];
    if admin == 0 && pointer == 0x8001 && old_tag == tag {
        return Ok(());
    }
    let result = (|| {
        session.set_attributes(340, instance, &association)?;
        session.verify_attributes(340, instance, &association)?;
        session.set_attributes(340, instance, &[(1, AttributeValue::number(0))])?;
        session.verify_attributes(
            340,
            instance,
            &[
                (1, AttributeValue::number(0)),
                association[0].clone(),
                association[1].clone(),
            ],
        )
    })();
    if let Err(primary) = result {
        if let Err(restore) = session
            .set_attributes(340, instance, &original)
            .and_then(|()| session.verify_attributes(340, instance, &original))
        {
            return Err(io::Error::new(
                primary.kind(),
                format!("{primary}; management association restore failed: {restore}"),
            ));
        }
        return Err(primary);
    }
    Ok(())
}
fn management_tag(vlan: u32, priority: u32) -> io::Result<[u8; 2]> {
    if vlan > 4094 || priority != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "management VLAN priority contract only verified at zero",
        ));
    }
    Ok(if vlan == 0 { 0xffff } else { vlan as u16 }.to_be_bytes())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pon_engine::omci::{session::OmciTransport, wire::BaselineFrame};
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    struct Step {
        action: u8,
        class: u16,
        request: Vec<u8>,
        result: u8,
        data: Vec<u8>,
    }
    struct Script {
        steps: Arc<Mutex<VecDeque<Step>>>,
        pending: Option<BaselineFrame>,
    }
    impl OmciTransport for Script {
        fn send(&mut self, request: &BaselineFrame) -> io::Result<()> {
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(
                (request.message_type, request.class_id),
                (step.action, step.class)
            );
            assert_eq!(
                request.instance_id,
                match step.class {
                    157 => 0x200,
                    137 => 0x8001,
                    _ => 0x401,
                }
            );
            let mut expected = step.request.clone();
            expected.resize(32, 0);
            assert_eq!(request.payload.as_slice(), expected);
            let mut response = *request;
            response.message_type = step.action & 0x1f | 0x20;
            response.payload = [0; 32];
            response.payload[0] = step.result;
            if step.action == 0x49 {
                response.payload[1..3].copy_from_slice(&step.request[..2]);
                response.payload[3..3 + step.data.len()].copy_from_slice(&step.data);
            }
            self.pending = Some(response);
            Ok(())
        }
        fn receive(&mut self) -> io::Result<Option<BaselineFrame>> {
            Ok(self.pending.take())
        }
    }
    fn step(action: u8, class: u16, request: &[u8], result: u8, data: &[u8]) -> Step {
        Step {
            action,
            class,
            request: request.to_vec(),
            result,
            data: data.to_vec(),
        }
    }
    #[test]
    fn configured_acs_auth_rejects_unverified_association() {
        assert!(super::ensure_auth_association_supported("").is_ok());
        assert_eq!(
            super::ensure_auth_association_supported("configured-user")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
    #[test]
    fn management_failure_restores_and_verifies_original_association() {
        let url = "http://acs.invalid/";
        let mut part = vec![0; 25];
        part[..url.len()].copy_from_slice(url.as_bytes());
        let steps = Arc::new(Mutex::new(VecDeque::from([
            step(0x49, 340, &[0xe0, 0], 0, &[1, 255, 255, 255, 255]),
            step(0x49, 157, &[0x80, 0], 0, &[1]),
            step(0x49, 157, &[0x40, 0], 0, &part),
            step(0x49, 137, &[0xc0, 0], 0, &[0, 0, 2, 0]),
            step(0x48, 340, &[0x60, 0, 0x80, 1, 0, 46], 0, &[]),
            // An accepted SET with stale readback must trigger restoration.
            step(0x49, 340, &[0x60, 0], 0, &[255, 255, 255, 255]),
            step(0x48, 340, &[0xe0, 0, 1, 255, 255, 255, 255], 0, &[]),
            step(0x49, 340, &[0xe0, 0], 0, &[1, 255, 255, 255, 255]),
        ])));
        let mut session = OmciSession::new(Script {
            steps: steps.clone(),
            pending: None,
        });
        assert!(configure(&mut session, 0x401, url, 46, 0)
            .unwrap_err()
            .to_string()
            .contains("readback mismatch"));
        assert!(steps.lock().unwrap().is_empty());
    }
    #[test]
    fn standard_management_tag_rejects_reserved_vlan_and_unverified_priority() {
        assert_eq!(super::management_tag(0, 0).unwrap(), [255, 255]);
        assert_eq!(super::management_tag(46, 0).unwrap(), [0, 46]);
        assert!(super::management_tag(4095, 0).is_err());
        assert!(super::management_tag(46, 1).is_err());
    }
}
pub fn bootstrap(session: &mut OmciSession, onu: u8, instance: u16) {
    let serial = super::service::onu_serial(onu);
    let Some(serial) = serial else {
        return;
    };
    let Some((desired, revision)) = crate::provisioning::management_desired(&serial) else {
        return;
    };
    let result = configure(
        session,
        instance,
        &desired.acs_url,
        desired.vlan,
        desired.priority,
    );
    crate::provisioning::management_result(
        &serial,
        serde_json::json!({"revision":revision,"state":if result.is_ok(){"association_verified"}else{"blocked"},"reason":result.err().map(|e|e.to_string()),"vlan":desired.vlan,"updated_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()}),
    );
}
