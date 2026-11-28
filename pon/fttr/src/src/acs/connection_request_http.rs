use crate::provisioning::model::ConnectionRequest;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::{header, Request, StatusCode};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Outcome {
    Accepted { http_status: u16 },
    Rejected { http_status: u16 },
    InvalidUrl,
    MissingCredentials,
    DefaultCredentialsUnavailable,
    TransportError,
    ResponseTooLarge,
    Requesting,
    Unauthenticated,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Target {
    scheme: String,
    address: SocketAddr,
    suffix: String,
}

impl Target {
    pub(super) fn effective(mut self, config: &ConnectionRequest) -> Self {
        self.address.set_port(config.port.get());
        self
    }
    pub(super) fn parse(url: &str, peer: &str) -> Result<Self, Outcome> {
        if url.len() > 2048
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
            || url.contains(['@', '#', '\\'])
        {
            return Err(Outcome::InvalidUrl);
        }
        let (scheme, rest) = url.split_once("://").ok_or(Outcome::InvalidUrl)?;
        let default_port = match scheme {
            "http" => 80,
            "https" => 443,
            _ => return Err(Outcome::InvalidUrl),
        };
        let boundary = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..boundary];
        let address = match authority.parse::<SocketAddr>() {
            Ok(address) => address,
            Err(_) => {
                let host = authority
                    .strip_prefix('[')
                    .and_then(|h| h.strip_suffix(']'))
                    .unwrap_or(authority);
                let ip = host.parse::<IpAddr>().map_err(|_| Outcome::InvalidUrl)?;
                if ip.is_ipv6() && !authority.starts_with('[') {
                    return Err(Outcome::InvalidUrl);
                }
                SocketAddr::new(ip, default_port)
            }
        };
        if address.port() == 0 || peer.parse::<IpAddr>().ok() != Some(address.ip()) {
            return Err(Outcome::InvalidUrl);
        }
        let suffix = &rest[boundary..];
        let slash = if suffix.starts_with('/') { "" } else { "/" };
        Ok(Self {
            scheme: scheme.into(),
            address,
            suffix: format!("{slash}{suffix}"),
        })
    }
}

#[cfg(test)]
pub(super) fn request(target: &Target, credentials: &ConnectionRequest) -> Outcome {
    crate::runtime::block_on(request_async(target, credentials))
}

pub(super) async fn request_async(target: &Target, credentials: &ConnectionRequest) -> Outcome {
    if credentials.defaults {
        return Outcome::DefaultCredentialsUnavailable;
    }
    if !credentials.credentials_available()
        || !credentials
            .authorization
            .load(std::sync::atomic::Ordering::Acquire)
    {
        return Outcome::MissingCredentials;
    }
    let mut address = target.address;
    address.set_port(credentials.port.get());
    let url = format!("{}://{}{}", target.scheme, address, target.suffix);
    tokio::time::timeout(
        Duration::from_secs(5),
        exchange(&url, &target.suffix, credentials),
    )
    .await
    .unwrap_or(Outcome::TransportError)
}

type HttpClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    Empty<Bytes>,
>;

fn client() -> &'static HttpClient {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(Duration::from_secs(2)));
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        Client::builder(TokioExecutor::new())
            .http1_title_case_headers(true)
            .http1_max_buf_size(16 * 1024)
            .retry_canceled_requests(false)
            .pool_max_idle_per_host(0)
            .build(https)
    })
}

async fn get(
    url: &str,
    authorization: Option<String>,
) -> Result<(StatusCode, hyper::HeaderMap), Outcome> {
    let mut request = Request::get(url);
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    let request = request
        .body(Empty::new())
        .map_err(|_| Outcome::InvalidUrl)?;
    let response = client()
        .request(request)
        .await
        .map_err(|_| Outcome::TransportError)?;
    let (parts, body) = response.into_parts();
    if parts
        .headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.len())
        .sum::<usize>()
        > 8192
        || parts
            .headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .is_some_and(|size| size > 8192)
    {
        return Err(Outcome::ResponseTooLarge);
    }
    Limited::new(body, 8192).collect().await.map_err(|error| {
        if error.is::<http_body_util::LengthLimitError>() {
            Outcome::ResponseTooLarge
        } else {
            Outcome::TransportError
        }
    })?;
    Ok((parts.status, parts.headers))
}

async fn exchange(url: &str, path: &str, credentials: &ConnectionRequest) -> Outcome {
    let (status, headers) = match get(url, None).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    if status != StatusCode::UNAUTHORIZED {
        return if matches!(status.as_u16(), 200 | 204) {
            Outcome::Unauthenticated
        } else {
            Outcome::Rejected {
                http_status: status.as_u16(),
            }
        };
    }
    let prompt = headers
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| digest_auth::parse(v).ok());
    let Some(mut prompt) = prompt else {
        return Outcome::Rejected { http_status: 401 };
    };
    let context = digest_auth::AuthContext::new(&credentials.username, &credentials.password, path);
    let authorization = match prompt.respond(&context) {
        Ok(answer) => answer.to_string(),
        Err(_) => return Outcome::TransportError,
    };
    if !credentials
        .authorization
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Outcome::MissingCredentials;
    }
    match get(url, Some(authorization)).await {
        Ok((status, _)) if matches!(status.as_u16(), 200 | 204) => Outcome::Accepted {
            http_status: status.as_u16(),
        },
        Ok((status, _)) => Outcome::Rejected {
            http_status: status.as_u16(),
        },
        Err(outcome) => outcome,
    }
}
