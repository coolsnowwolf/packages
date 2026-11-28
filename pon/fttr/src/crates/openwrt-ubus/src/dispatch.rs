use serde_json::Value;

pub(crate) fn parse_request(encoded: Option<&str>, present: bool) -> Result<Option<Value>, i32> {
    if !present {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(encoded.ok_or(2)?).map_err(|_| 2)?;
    if !value.is_object() {
        return Err(2);
    }
    Ok(Some(value))
}

pub(crate) fn reply_status(
    result: Result<Value, i32>,
    encode: impl FnOnce(&Value) -> Result<String, ()>,
    send: impl FnOnce(&str) -> Result<(), i32>,
) -> i32 {
    match result {
        Err(code) => code,
        Ok(value) if value.is_null() => 0,
        Ok(value) => match encode(&value) {
            Err(()) => 9,
            Ok(json) => match send(&json) {
                Ok(()) => 0,
                Err(code) if code > 0 => code,
                Err(_) => 9,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_request_is_never_treated_as_empty() {
        assert_eq!(parse_request(None, false).unwrap(), None);
        for input in [None, Some("not-json"), Some("null"), Some("[]")] {
            assert_eq!(parse_request(input, true).unwrap_err(), 2);
        }
        assert!(parse_request(Some("{}"), true)
            .unwrap()
            .unwrap()
            .is_object());
    }
    #[test]
    fn encoding_blob_and_send_failures_never_report_success() {
        let value = || Ok(serde_json::json!({"success":true}));
        assert_eq!(
            reply_status(value(), |_| Err(()), |_| panic!("must not send")),
            9
        );
        assert_eq!(reply_status(value(), |_| Ok("{}".into()), |_| Err(9)), 9);
        assert_eq!(reply_status(value(), |_| Ok("{}".into()), |_| Err(-1)), 9);
        assert_eq!(reply_status(value(), |_| Ok("{}".into()), |_| Err(7)), 7);
        assert_eq!(
            reply_status(
                Err(2),
                |_| panic!("must not encode"),
                |_| panic!("must not send")
            ),
            2
        );
        assert_eq!(reply_status(value(), |_| Ok("{}".into()), |_| Ok(())), 0);
    }
}
