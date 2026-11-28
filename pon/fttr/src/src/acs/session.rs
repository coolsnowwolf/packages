//! CWMP session semantics, independent of HTTP framing and socket ownership.
use super::{
    connection_request_proof, error_result,
    http::{Reply, Request},
    now, soap, token, Device, Server, TaskState,
};
use std::io;
use std::sync::atomic::Ordering;

pub(super) struct Session {
    peer: String,
    device: Option<String>,
    cookie: Option<String>,
    requests: usize,
}

impl Session {
    pub(super) fn new(peer: String) -> Self {
        Self {
            peer,
            device: None,
            cookie: None,
            requests: 0,
        }
    }

    pub(super) fn handle(&mut self, server: &Server, req: Request) -> Reply {
        match self.process(server, req) {
            Ok(reply) => reply,
            Err(error) => {
                eprintln!("[ACS] peer={} CWMP request rejected: {error}", self.peer);
                Reply::empty(400)
            }
        }
    }

    fn process(&mut self, server: &Server, req: Request) -> io::Result<Reply> {
        self.requests += 1;
        server.requests.fetch_add(1, Ordering::Relaxed);
        let matches = |s: &Server| {
            super::path_allowed(s, &req.path)
                && s.auth
                    .as_ref()
                    .is_none_or(|auth| req.headers.get("authorization") == Some(auth))
        };
        let legacy = if matches(server) {
            None
        } else {
            server
                .legacy
                .lock()
                .unwrap()
                .iter()
                .find(|s| matches(s))
                .cloned()
        };
        let policy = legacy.as_deref().unwrap_or(server);
        if !super::path_allowed(policy, &req.path) {
            return Ok(Reply::empty(404));
        }
        if !matches(policy) {
            return Ok(Reply::empty(401));
        }
        let mut state = server.state.lock().unwrap();
        state.expire();
        // Revalidate even an established connection after reset/session expiry.
        if self
            .cookie
            .as_ref()
            .is_some_and(|key| !state.sessions.contains_key(key))
        {
            self.device = None;
            self.cookie = None;
        }
        if self.device.is_none() {
            let cookie = req.headers.get("cookie").and_then(|value| {
                value
                    .split(';')
                    .find_map(|part| part.trim().strip_prefix("fttr_session="))
            });
            self.device = cookie
                .and_then(|key| state.sessions.get_mut(key))
                .filter(|(_, peer, _)| *peer == self.peer)
                .map(|(device, _, at)| {
                    *at = now();
                    device.clone()
                });
            if self.device.is_some() {
                self.cookie = cookie.map(str::to_owned);
            }
        }
        let mut cookie = None;
        let mut dispatch = None;
        let mut changed = false;
        let response = if req.message.is_none() {
            let Some(device) = &self.device else {
                return Ok(Reply::empty(400));
            };
            let Some(known) = state.devices.get(device) else {
                return Ok(Reply::empty(400));
            };
            let namespace = known.namespace.clone();
            state.next(device, &namespace)?.map(|rpc| {
                dispatch = Some(rpc.undo);
                changed = true;
                rpc.xml
            })
        } else {
            let message = req.message.unwrap();
            match message.method.as_str() {
                "Inform" => {
                    if message.serial.is_empty() || message.serial.len() > 128
                        || message.oui.len() != 6 || !message.oui.bytes().all(|b| b.is_ascii_hexdigit())
                        || message.product.len() > 128
                    { return Ok(Reply::empty(400)); }
                    let device = format!("{}:{}:{}", message.oui.to_ascii_uppercase(), message.product, message.serial);
                    if state.devices.len() >= 64 && !state.devices.contains_key(&device) {
                        return Ok(Reply::empty(503));
                    }
                    if server.stopping.load(Ordering::Acquire) { return Ok(Reply::empty(503)); }
                    let key = token()?;
                    for task in state.tasks.values_mut().filter(|t| t.device == device && t.state == TaskState::Sent) {
                        task.state = TaskState::Unknown;
                        task.result = Some(error_result("new Inform before RPC response"));
                        task.release_request();
                    }
                    state.sessions.retain(|_, (id, _, _)| *id != device);
                    state.sessions.insert(key.clone(), (device.clone(), self.peer.clone(), now()));
                    cookie = Some(format!("fttr_session={key}; Path={}; HttpOnly; SameSite=Strict", super::cookie_path(policy)));
                    state.devices.insert(device.clone(), Device {
                        serial: message.serial, oui: message.oui, product_class: message.product,
                        peer: self.peer.clone(), namespace: message.namespace.clone(), last_inform: now(),
                        endpoint_id: policy.endpoint_id.clone(), inform: serde_json::to_value(message.data).map_err(io::Error::other)?,
                    });
                    if let Some(mut proof) = state.connection_request_proofs.remove(&device) {
                        proof.inform(&state.devices[&device], &key);
                        if proof.current(&state.devices[&device], &proof.config()) {
                            state.connection_request_proofs.insert(device.clone(), proof);
                        }
                    }
                    self.cookie = Some(key);
                    crate::provisioning::controller::wake_device(&device);
                    self.device = Some(device);
                    super::connection_request::wake();
                    changed = true;
                    Some(soap::envelope(&message.namespace, &message.id,
                        "<cwmp:InformResponse><MaxEnvelopes>1</MaxEnvelopes></cwmp:InformResponse>"))
                }
                "GetRPCMethods" => Some(soap::envelope(&message.namespace, &message.id,
                    "<cwmp:GetRPCMethodsResponse><MethodList soap-enc:arrayType=\"xsd:string[3]\"><string>Inform</string><string>GetRPCMethods</string><string>TransferComplete</string></MethodList></cwmp:GetRPCMethodsResponse>")),
                "TransferComplete" if self.device.is_some() => Some(soap::envelope(
                    &message.namespace, &message.id, "<cwmp:TransferCompleteResponse/>")),
                _ => {
                    let Some(device) = &self.device else { return Ok(Reply::empty(400)); };
                    let namespace = message.namespace.clone();
                    state.response(device, message)?;
                    if let Some(session) = &self.cookie {
                        connection_request_proof::completed(&mut state, device, session);
                    }
                    changed = true;
                    state.next(device, &namespace)?.map(|rpc| { dispatch = Some(rpc.undo); rpc.xml })
                }
            }
        };
        if changed && server.save(&state).is_err() {
            if let Some(undo) = dispatch {
                undo.restore(&mut state);
            }
            return Ok(Reply::empty(503));
        }
        if let (Some(device), Some(session), Some(undo)) = (&self.device, &self.cookie, &dispatch) {
            connection_request_proof::dispatched(&mut state, device, session, undo.id);
        }
        let close = response.is_none()
            || self.requests >= 64
            || req
                .headers
                .get("connection")
                .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        Ok(Reply {
            code: if response.is_some() { 200 } else { 204 },
            body: response.unwrap_or_default(),
            cookie,
            close,
        })
    }
}
