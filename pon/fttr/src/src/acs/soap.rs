use serde_json::Value;
use std::fmt::Write as _;
use std::io;

pub const SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub(super) const MAX_SET_PARAMETERS: usize = 32;
pub(super) struct Parameter<'a> {
    pub(super) name: &'a str,
    pub(super) value: &'a str,
    pub(super) kind: &'a str,
}

pub(super) enum WriteArgs<'a> {
    Set(Vec<Parameter<'a>>),
    Add(&'a str),
    Delete(&'a str),
}

impl<'a> WriteArgs<'a> {
    pub(super) fn parse(method: &str, args: &'a Value) -> io::Result<Option<Self>> {
        let write = match method {
            "SetParameterValues" => Self::Set(set_parameters(args)?),
            "AddObject" | "DeleteObject" => {
                let object = args["object"]
                    .as_str()
                    .filter(|s| !s.is_empty() && valid_path(s))
                    .ok_or_else(|| bad("invalid CWMP object name"))?;
                if method == "AddObject" {
                    Self::Add(object)
                } else {
                    Self::Delete(object)
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(write))
    }

    pub(super) fn request(&self, namespace: &str, id: u64, parameter_key: &str) -> String {
        let parameter_key = escape(parameter_key);
        let body = match self {
            Self::Set(values) => {
                let mut list = String::new();
                for p in values {
                    let _ = write!(list, "<ParameterValueStruct><Name>{}</Name><Value xsi:type=\"xsd:{}\">{}</Value></ParameterValueStruct>", escape(p.name), p.kind, escape(p.value));
                }
                format!("<cwmp:SetParameterValues><ParameterList soap-enc:arrayType=\"cwmp:ParameterValueStruct[{}]\">{list}</ParameterList><ParameterKey>{parameter_key}</ParameterKey></cwmp:SetParameterValues>", values.len())
            }
            Self::Add(object) | Self::Delete(object) => {
                let method = if matches!(self, Self::Add(_)) {
                    "AddObject"
                } else {
                    "DeleteObject"
                };
                format!("<cwmp:{method}><ObjectName>{}</ObjectName><ParameterKey>{parameter_key}</ParameterKey></cwmp:{method}>", escape(object))
            }
        };
        envelope(namespace, &format!("fttr-{id}"), &body)
    }
}

pub(super) fn valid_path(path: &str) -> bool {
    path.len() <= 1024 && !path.chars().any(char::is_control)
}

pub(super) fn valid_value_type(kind: &str) -> bool {
    matches!(
        kind,
        "string"
            | "boolean"
            | "int"
            | "unsignedInt"
            | "dateTime"
            | "base64"
            | "hexBinary"
            | "long"
            | "unsignedLong"
    )
}

pub(super) fn set_parameters(args: &Value) -> io::Result<Vec<Parameter<'_>>> {
    let values = args["parameters"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= MAX_SET_PARAMETERS)
        .ok_or_else(|| bad("invalid SetParameterValues parameter list"))?;
    let mut parameters = Vec::with_capacity(values.len());
    for item in values {
        let name = item["name"]
            .as_str()
            .filter(|s| !s.is_empty() && valid_path(s))
            .ok_or_else(|| bad("invalid SetParameterValues parameter name"))?;
        if parameters.iter().any(|p: &Parameter<'_>| p.name == name) {
            return Err(bad("duplicate SetParameterValues parameter name"));
        }
        let value = item["value"]
            .as_str()
            .filter(|s| {
                s.len() <= 4096
                    && !s
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            })
            .ok_or_else(|| bad("invalid SetParameterValues parameter value"))?;
        let kind = match item.get("kind") {
            None => "string",
            Some(kind) => kind
                .as_str()
                .filter(|kind| valid_value_type(kind))
                .ok_or_else(|| bad("unsupported XSD type"))?,
        };
        parameters.push(Parameter { name, value, kind });
    }
    Ok(parameters)
}
#[derive(Debug)]
pub struct Message {
    pub method: String,
    pub namespace: String,
    pub id: String,
    pub serial: String,
    pub oui: String,
    pub product: String,
    pub data: crate::cwmp::response::Data,
}
fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
#[cfg(test)]
pub fn parse(input: &str) -> io::Result<Message> {
    crate::runtime::block_on(parse_stream(
        tokio::io::BufReader::new(input.as_bytes()),
        |_| super::resources::RESULT_BYTES,
    ))
}

/// Pull parsing keeps only an element stack and the bounded result. Input never
/// becomes a complete XML document and partial results cannot reach State.
pub(super) async fn parse_stream<R: tokio::io::AsyncBufRead + Unpin>(
    input: R,
    mut limit: impl FnMut(&str) -> usize,
) -> io::Result<Message> {
    use quick_xml::{events::Event, name::ResolveResult, NsReader, XmlVersion};
    use serde_json::{json, Value};
    let mut reader = NsReader::from_reader(input);
    reader.config_mut().expand_empty_elements = true;
    let mut buffer = Vec::with_capacity(4096);
    let mut stack: Vec<String> = Vec::new();
    let mut message = Message {
        method: String::new(),
        namespace: String::new(),
        id: String::new(),
        serial: String::new(),
        oui: String::new(),
        product: String::new(),
        data: crate::cwmp::response::Data {
            parameters: Some(Vec::new()),
            fields: [("methods".into(), json!([])), ("events".into(), json!([]))].into(),
        },
    };
    let mut row = None;
    let mut text = String::new();
    let mut nodes = 0usize;
    let mut result_bytes = message.data.bytes();
    let mut ceiling = 128 * 1024;
    let mut envelope = false;
    let mut body = false;
    let mut header = false;
    let mut id_seen = false;
    let mut xml_version = XmlVersion::Implicit1_0;
    loop {
        let (namespace, event) = reader
            .read_resolved_event_into_async(&mut buffer)
            .await
            .map_err(|e| bad(&format!("invalid SOAP XML: {e}")))?;
        match event {
            Event::Start(e) => {
                nodes += 1;
                if nodes > 120_000 || stack.len() >= 64 {
                    return Err(bad("SOAP structure exceeds bound"));
                }
                let name = e.local_name().as_ref().to_owned();
                let ns = match &namespace {
                    ResolveResult::Bound(ns) => ns.as_ref(),
                    _ => "",
                };
                if stack.is_empty() {
                    if envelope || name != "Envelope" || ns != SOAP {
                        return Err(bad("not one SOAP 1.1 envelope"));
                    }
                    envelope = true;
                } else if stack.len() == 1 && name == "Header" {
                    if header || body || ns != SOAP {
                        return Err(bad("invalid SOAP Header"));
                    }
                    header = true;
                } else if stack.len() == 1 && name == "Body" {
                    if body || ns != SOAP {
                        return Err(bad("invalid SOAP Body"));
                    }
                    body = true;
                } else if stack.len() == 1 {
                    return Err(bad("unexpected SOAP envelope child"));
                } else if stack.len() == 2 && stack[1] == "Body" {
                    if !message.method.is_empty() {
                        return Err(bad("multiple SOAP RPCs"));
                    }
                    message.method = name.clone();
                    if name == "Fault" && ns == SOAP {
                        message.namespace = "urn:dslforum-org:cwmp-1-0".into();
                    } else if matches!(
                        ns,
                        "urn:dslforum-org:cwmp-1-0"
                            | "urn:dslforum-org:cwmp-1-1"
                            | "urn:dslforum-org:cwmp-1-2"
                            | "urn:dslforum-org:cwmp-1-3"
                            | "urn:dslforum-org:cwmp-1-4"
                    ) {
                        message.namespace = ns.into();
                    } else {
                        return Err(bad("unsupported CWMP namespace"));
                    }
                    ceiling = limit(&message.id);
                }
                if name == "ID"
                    && (stack.len() != 2
                        || stack[1] != "Header"
                        || !matches!(
                            ns,
                            "urn:dslforum-org:cwmp-1-0"
                                | "urn:dslforum-org:cwmp-1-1"
                                | "urn:dslforum-org:cwmp-1-2"
                                | "urn:dslforum-org:cwmp-1-3"
                                | "urn:dslforum-org:cwmp-1-4"
                        ))
                {
                    return Err(bad("invalid CWMP ID"));
                }
                if matches!(
                    name.as_str(),
                    "ParameterValueStruct" | "ParameterInfoStruct" | "SetParameterValuesFault"
                ) {
                    if row.is_some() || stack.get(1).map(String::as_str) != Some("Body") {
                        return Err(bad("invalid parameter row"));
                    }
                    if name != "SetParameterValuesFault"
                        && stack.last().map(String::as_str) != Some("ParameterList")
                    {
                        return Err(bad("parameter row outside ParameterList"));
                    }
                    row = Some(serde_json::Map::new());
                }
                if matches!(
                    stack.last().map(String::as_str),
                    Some("Value" | "Name" | "Writable" | "Status" | "InstanceNumber" | "ID")
                ) {
                    return Err(bad("nested scalar field"));
                }
                if name == "Value" {
                    for attr in e.attributes() {
                        let attr = attr.map_err(|_| bad("invalid XML attribute"))?;
                        let (ns, local) = reader.resolver().resolve_attribute(attr.key);
                        if local.as_ref() == "type"
                            && matches!(ns, ResolveResult::Bound(n) if n.as_ref() == "http://www.w3.org/2001/XMLSchema-instance")
                        {
                            if let Some(row) = &mut row {
                                row.insert(
                                    "type".into(),
                                    Value::String(
                                        attr.normalized_value(xml_version)
                                            .map_err(|_| bad("invalid XML type"))?
                                            .into_owned(),
                                    ),
                                );
                            }
                        }
                    }
                }
                stack.push(name);
                text.clear();
            }
            Event::Text(e) => {
                let decoded = e.xml_content(xml_version);
                if stack.is_empty() && !decoded.trim().is_empty() {
                    return Err(bad("text outside SOAP envelope"));
                }
                text.push_str(
                    &quick_xml::escape::unescape(&decoded)
                        .map_err(|_| bad("invalid XML entity"))?,
                );
                if text.len() > 16 * 1024 {
                    return Err(bad("SOAP field exceeds bound"));
                }
            }
            Event::CData(e) => {
                if stack.is_empty() {
                    return Err(bad("CDATA outside SOAP envelope"));
                }
                text.push_str(&e.xml_content(xml_version));
                if text.len() > 16 * 1024 {
                    return Err(bad("SOAP field exceeds bound"));
                }
            }
            Event::GeneralRef(e) => {
                let name = e.into_inner();
                let escaped = format!("&{name};");
                text.push_str(
                    &quick_xml::escape::unescape(&escaped)
                        .map_err(|_| bad("invalid XML reference"))?,
                );
                if text.len() > 16 * 1024 {
                    return Err(bad("SOAP field exceeds bound"));
                }
            }
            Event::End(_) => {
                let name = stack.pop().ok_or_else(|| bad("unexpected XML end"))?;
                if matches!(
                    name.as_str(),
                    "ParameterValueStruct" | "ParameterInfoStruct" | "SetParameterValuesFault"
                ) {
                    let value =
                        Value::Object(row.take().ok_or_else(|| bad("missing parameter row"))?);
                    if value["name"].as_str().is_none_or(str::is_empty)
                        || name == "ParameterValueStruct" && !value["value"].is_string()
                        || name == "ParameterInfoStruct"
                            && !matches!(
                                value["writable"].as_str(),
                                Some("true" | "false" | "1" | "0")
                            )
                    {
                        return Err(bad("incomplete parameter row"));
                    }
                    let value: crate::cwmp::response::Row =
                        serde_json::from_value(value).map_err(|_| bad("invalid parameter row"))?;
                    result_bytes +=
                        value.heap_bytes() + std::mem::size_of::<crate::cwmp::response::Row>();
                    let rows = message.data.parameters.as_mut().unwrap();
                    if rows.len() >= 32768 || result_bytes > ceiling {
                        return Err(bad("response_too_large: parameter result exceeds contract"));
                    }
                    if rows.len() == rows.capacity() {
                        rows.reserve_exact(64);
                    }
                    rows.push(value);
                } else if let Some(row) = &mut row {
                    let key = match name.as_str() {
                        "Name" | "ParameterName" => Some("name"),
                        "Value" => Some("value"),
                        "Writable" => Some("writable"),
                        "FaultCode" => Some("fault_code"),
                        "FaultString" => Some("fault_string"),
                        _ => None,
                    };
                    if let Some(key) = key {
                        if row
                            .insert(key.into(), Value::String(std::mem::take(&mut text)))
                            .is_some()
                        {
                            return Err(bad("duplicate parameter field"));
                        }
                    }
                } else {
                    match name.as_str() {
                        "ID" => {
                            if id_seen {
                                return Err(bad("duplicate CWMP ID"));
                            }
                            id_seen = true;
                            message.id = std::mem::take(&mut text);
                        }
                        "SerialNumber" if stack.last().map(String::as_str) == Some("DeviceId") => {
                            message.serial = std::mem::take(&mut text)
                        }
                        "OUI" if stack.last().map(String::as_str) == Some("DeviceId") => {
                            message.oui = std::mem::take(&mut text)
                        }
                        "ProductClass" if stack.last().map(String::as_str) == Some("DeviceId") => {
                            message.product = std::mem::take(&mut text)
                        }
                        "string" | "EventCode" => {
                            let key = if name == "string" {
                                "methods"
                            } else {
                                "events"
                            };
                            result_bytes += text.capacity() + 64;
                            message.data[key]
                                .as_array_mut()
                                .unwrap()
                                .push(Value::String(std::mem::take(&mut text)));
                        }
                        "Status" | "InstanceNumber" | "FaultCode" | "FaultString" => {
                            if stack.get(1).map(String::as_str) != Some("Body") {
                                return Err(bad("response field outside SOAP Body"));
                            }
                            let key = match name.as_str() {
                                "Status" => "status",
                                "InstanceNumber" => "instance_number",
                                "FaultCode" => "fault_code",
                                _ => "fault_string",
                            };
                            result_bytes += text.capacity() + 128;
                            if message
                                .data
                                .fields
                                .insert(key.into(), Value::String(std::mem::take(&mut text)))
                                .is_some()
                            {
                                return Err(bad("duplicate SOAP response field"));
                            }
                        }
                        _ => (),
                    }
                    if result_bytes > ceiling {
                        return Err(bad("response_too_large: SOAP result exceeds contract"));
                    }
                }
                text.clear();
            }
            Event::Decl(e) => {
                xml_version = e.xml_version().map_err(|_| bad("invalid XML version"))?;
            }
            Event::DocType(_) => return Err(bad("DTD is disabled")),
            Event::Eof => break,
            _ => (),
        }
        buffer.clear();
    }
    if !stack.is_empty() || !body || message.method.is_empty() {
        return Err(bad("incomplete SOAP envelope"));
    }
    if message.data.bytes() > ceiling {
        return Err(bad("response_too_large: result capacity exceeds contract"));
    }
    Ok(message)
}
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn envelope(namespace: &str, id: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\"?><soap:Envelope xmlns:soap=\"{SOAP}\" xmlns:cwmp=\"{}\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" xmlns:soap-enc=\"http://schemas.xmlsoap.org/soap/encoding/\"><soap:Header><cwmp:ID soap:mustUnderstand=\"1\">{}</cwmp:ID></soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>", escape(namespace), escape(id))
}
pub(super) fn request(namespace: &str, id: u64, method: &str, args: &Value) -> io::Result<String> {
    let body = match method {
        "GetRPCMethods" => "<cwmp:GetRPCMethods/>".into(),
        "GetParameterNames" => format!("<cwmp:GetParameterNames><ParameterPath>{}</ParameterPath><NextLevel>{}</NextLevel></cwmp:GetParameterNames>", escape(args["path"].as_str().unwrap_or("")), args["next_level"].as_bool().unwrap_or(false)),
        "GetParameterValues" => {
            let names = args["names"].as_array().ok_or_else(|| bad("names array required"))?;
            format!("<cwmp:GetParameterValues><ParameterNames soap-enc:arrayType=\"xsd:string[{}]\">{}</ParameterNames></cwmp:GetParameterValues>", names.len(), names.iter().map(|n| format!("<string>{}</string>", escape(n.as_str().unwrap_or("")))).collect::<String>())
        }
        _ => return Err(bad("unsupported ACS RPC")),
    };
    Ok(envelope(namespace, &format!("fttr-{id}"), &body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_oversized_response_never_returns_partial_rows() {
        let good =
            "<ParameterInfoStruct><Name>x</Name><Writable>true</Writable></ParameterInfoStruct>";
        for bad in [
            "<ParameterInfoStruct><Name>x</Name></ParameterInfoStruct>",
            "<ParameterInfoStruct><Name>x</Name><Name>y</Name><Writable>true</Writable></ParameterInfoStruct>",
            "<ParameterInfoStruct><Name>x</Name><Writable>yes</Writable></ParameterInfoStruct>",
            "<ParameterValueStruct><Name>x</Name><Value><nested/></Value></ParameterValueStruct>",
        ] {
            let xml = envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", &format!("<cwmp:GetParameterNamesResponse><ParameterList>{good}{bad}</ParameterList></cwmp:GetParameterNamesResponse>"));
            assert!(parse(&xml).is_err(), "accepted malformed row: {bad}");
        }
        let xml = envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", &format!("<cwmp:GetParameterNamesResponse><ParameterList>{good}</ParameterList></cwmp:GetParameterNamesResponse>"));
        assert!(parse(&xml[..xml.len() - 10]).is_err());
        assert!(crate::runtime::block_on(parse_stream(
            tokio::io::BufReader::new(xml.as_bytes()),
            |_| 128
        ))
        .is_err());
        let duplicate = envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", "<cwmp:SetParameterValuesResponse><Status>0</Status><Status>1</Status></cwmp:SetParameterValuesResponse>");
        assert!(parse(&duplicate).is_err());
        assert!(parse(&format!("{xml}<![CDATA[extra]]>")).is_err());
    }
    #[test]
    fn object_response_and_parameter_fault() {
        let reply=parse(&envelope("urn:dslforum-org:cwmp-1-0","fttr-2","<cwmp:AddObjectResponse><InstanceNumber>4</InstanceNumber><Status>1</Status></cwmp:AddObjectResponse>")).unwrap();
        assert_eq!(reply.data["instance_number"], "4");
        assert_eq!(reply.data["status"], "1");
        let fault=parse(&envelope("urn:dslforum-org:cwmp-1-0","fttr-2","<soap:Fault><detail><cwmp:Fault><FaultCode>9003</FaultCode><SetParameterValuesFault><ParameterName>InternetGatewayDevice.x</ParameterName><FaultCode>9005</FaultCode></SetParameterValuesFault></cwmp:Fault></detail></soap:Fault>")).unwrap();
        assert_eq!(
            fault.data.rows().unwrap()[0]["name"],
            "InternetGatewayDevice.x"
        );
        let xml = WriteArgs::parse(
            "DeleteObject",
            &serde_json::json!({"object":"InternetGatewayDevice.WANDevice.1.WANConnectionDevice.4."}),
        ).unwrap().unwrap().request("urn:dslforum-org:cwmp-1-0", 2, "fixture-key");
        assert_eq!(parse(&xml).unwrap().method, "DeleteObject");
    }
    #[test]
    fn namespaces_entities_and_faults() {
        let input = envelope("urn:dslforum-org:cwmp-1-2", "id<&", "<cwmp:Inform><DeviceId><OUI>AABBCC</OUI><SerialNumber>SN&amp;1</SerialNumber></DeviceId><ParameterList><ParameterValueStruct><Name>x</Name><Value xsi:type=\"xsd:string\">a&lt;b</Value></ParameterValueStruct></ParameterList></cwmp:Inform>");
        let parsed = parse(&input).unwrap();
        assert_eq!(parsed.serial, "SN&1");
        assert_eq!(parsed.id, "id<&");
        assert_eq!(parsed.data.rows().unwrap()[0]["value"], "a<b");
        assert!(parse("<!DOCTYPE x [<!ENTITY a 'b'>]><x/>").is_err());
        let fault = parse(&envelope("urn:dslforum-org:cwmp-1-0", "x", "<soap:Fault><detail><cwmp:Fault><FaultCode>9005</FaultCode><FaultString>Unknown</FaultString></cwmp:Fault></detail></soap:Fault>")).unwrap();
        assert_eq!(fault.data["fault_code"], "9005");
    }
    #[test]
    fn typed_write_is_escaped() {
        let xml = WriteArgs::parse(
            "SetParameterValues",
            &serde_json::json!({"parameters":[{"name":"x", "value":"<ssid>&", "kind":"string"}]}),
        )
        .unwrap()
        .unwrap()
        .request("urn:dslforum-org:cwmp-1-0", 1, "fixture-key");
        assert!(xml.contains("&lt;ssid&gt;&amp;"));
        assert!(parse(&xml).is_ok());
        assert!(WriteArgs::parse(
            "SetParameterValues",
            &serde_json::json!({"parameters":[{"name":"x", "value":"a", "kind":"evil"}]}),
        )
        .is_err());
    }
    #[test]
    fn full_parameter_tree_exceeding_old_limits() {
        let rows = (0..6000).map(|i| format!("<ParameterInfoStruct><Name>InternetGatewayDevice.Test.Parameter{i}</Name><Writable>true</Writable></ParameterInfoStruct>")).collect::<String>();
        let xml = envelope("urn:dslforum-org:cwmp-1-0", "fttr-1", &format!("<cwmp:GetParameterNamesResponse><ParameterList soap-enc:arrayType=\"cwmp:ParameterInfoStruct[6000]\">{rows}</ParameterList></cwmp:GetParameterNamesResponse>"));
        assert!(xml.len() > 512 * 1024);
        let parsed = parse(&xml).unwrap();
        assert_eq!(parsed.data.rows().unwrap().len(), 6000);
        assert_eq!(parsed.data.rows().unwrap()[5999]["writable"], "true");
        let excessive = envelope(
            "urn:dslforum-org:cwmp-1-0",
            "fttr-1",
            &format!(
                "<cwmp:GetParameterNamesResponse>{}</cwmp:GetParameterNamesResponse>",
                "<x/>".repeat(120_000)
            ),
        );
        assert!(parse(&excessive).is_err());
    }
}
