//! Read compatibility must not silently become a vendor write encoding.
pub fn service_tokens(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', '_'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[derive(Clone, Copy, Debug)]
pub enum ServiceEncoding {
    Comma,
    Underscore,
}

/// Caller supplies an encoding only after the write contract is verified.
pub fn encode_services(
    tokens: &[&str],
    verified: Option<ServiceEncoding>,
) -> Result<String, &'static str> {
    if tokens.is_empty()
        || tokens
            .iter()
            .any(|t| t.is_empty() || t.contains([',', '_']) || t.trim() != *t)
    {
        return Err("invalid logical service token");
    }
    if tokens.len() == 1 {
        return Ok(tokens[0].to_owned());
    }
    match verified {
        Some(ServiceEncoding::Comma) => Ok(tokens.join(",")),
        Some(ServiceEncoding::Underscore) => Ok(tokens.join("_")),
        None => Err("combined ServiceList write encoding is unverified"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepting_read_delimiters_does_not_grant_combined_write_permission() {
        assert_eq!(
            service_tokens(" TR069_INTERNET,OTHER ").collect::<Vec<_>>(),
            ["TR069", "INTERNET", "OTHER"]
        );
        assert!(encode_services(&["INTERNET", "OTHER"], None).is_err());
        assert_eq!(
            encode_services(&["INTERNET", "OTHER"], Some(ServiceEncoding::Underscore)).unwrap(),
            "INTERNET_OTHER"
        );
        assert!(encode_services(&["TR069_INTERNET"], Some(ServiceEncoding::Comma)).is_err());
    }
}
