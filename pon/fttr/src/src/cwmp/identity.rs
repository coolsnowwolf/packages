//! Identity and cache scope are independent of the OMCI adapter enum.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub optical_vendor: String,
    pub optical_serial: String,
    pub cwmp_serial: String,
    pub product_class: String,
    pub software_version: String,
    pub hardware_version: String,
}

pub fn associated(identity: &Identity) -> bool {
    let sn = &identity.optical_serial;
    if sn.len() != 12 || !sn.is_ascii() {
        return false;
    }
    if identity.optical_vendor == "SCTY"
        && super::vendor::ct::matches_identity(sn, &identity.product_class, &identity.cwmp_serial)
    {
        return true;
    }
    let hex = format!(
        "{}{}",
        sn[..4]
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect::<String>(),
        &sn[4..]
    );
    if identity.optical_vendor == "H3CT" {
        identity
            .cwmp_serial
            .to_ascii_uppercase()
            .contains(&sn.to_ascii_uppercase())
    } else {
        identity.cwmp_serial.eq_ignore_ascii_case(&hex)
            || identity.cwmp_serial.eq_ignore_ascii_case(sn)
    }
}
pub fn specialization(identity: &Identity) -> Option<&'static super::contract::Contract> {
    if identity.optical_serial.is_empty() || identity.cwmp_serial.is_empty() {
        return None;
    }
    super::discovery::specialization(&identity.optical_vendor, &identity.product_class)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_native_identity_does_not_specialize_or_authorize_a_write_contract() {
        let mut identity = Identity {
            optical_vendor: "SCTY".into(),
            optical_serial: "SCTY72ABCDEF".into(),
            cwmp_serial: "308618C8172ABCDEF".into(),
            product_class: "TEWA-768G".into(),
            software_version: "Tianyi_V1.0.5".into(),
            hardware_version: "V3.0".into(),
        };
        assert!(associated(&identity));
        assert!(specialization(&identity).is_none());
        identity.optical_vendor = "HWTC".into();
        assert!(!associated(&identity));
        identity.optical_vendor = "SCTY".into();
        identity.product_class = "other".into();
        assert!(!associated(&identity));
        identity.cwmp_serial = "SCTY72ABCDEF".into();
        assert!(associated(&identity));
        identity.cwmp_serial = "5343545972ABCDEF".into();
        assert!(associated(&identity));
    }
}
