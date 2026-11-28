//! CWMP task lifecycle and retention, independent of socket framing.
use super::{error_result, now, protect_management, soap, transaction, State, TaskState};
use std::io;

impl State {
    #[cfg(test)]
    pub(super) fn forget_tasks(&mut self, device: &str) {
        self.tasks.retain(|_, task| task.device != device);
        self.sessions.retain(|_, (id, _, _)| id != device);
    }

    pub(super) fn prune(&mut self) {
        let time = now();
        self.tasks.retain(|_, task| {
            task.state.is_active()
                || task.unresolved_write()
                || time.saturating_sub(
                    self.results
                        .get(&task.id)
                        .and_then(|i| i.completed_at)
                        .unwrap_or(task.sent.unwrap_or(task.created)),
                ) < 180
        });
        self.results.retain(|id, _| self.tasks.contains_key(id));
        self.intents
            .retain(|id, _| self.tasks.get(id).is_some_and(|t| t.checkpoint_write()));
        for task in self.tasks.values_mut() {
            let mut legacy = super::retention::ResultInfo::default();
            let info = self.results.get_mut(&task.id).unwrap_or(&mut legacy);
            let age = time.saturating_sub(
                info.completed_at
                    .unwrap_or(task.sent.unwrap_or(task.created)),
            );
            if info.protected
                && info.completed_at.is_some()
                && age >= super::retention::LEASE_SECONDS
            {
                info.protected = false;
                info.reason = "consumer lease expired".into();
            }
            if !task.state.is_active() && !info.protected && age >= 15 {
                task.result = None;
                info.status = "expired".into();
                if info.reason.is_empty() {
                    info.reason = "ordinary response TTL".into();
                }
            }
        }
        let mut total_result_bytes: usize = self
            .tasks
            .values()
            .filter(|t| !self.results.get(&t.id).is_some_and(|i| i.protected))
            .filter_map(|t| t.result.as_ref())
            .map(|r| r.bytes())
            .sum();
        let protected_bytes: usize = self
            .tasks
            .values()
            .filter(|t| self.results.get(&t.id).is_some_and(|i| i.protected))
            .filter_map(|t| t.result.as_ref())
            .map(|r| r.bytes())
            .sum();
        // Reserve 1 MiB for task arguments, device Inform records and JSON metadata.
        let ordinary_ceiling = (7 * 1024 * 1024usize).saturating_sub(protected_bytes);
        if total_result_bytes > 256 * 1024 {
            let inactive_ids: Vec<u64> = self
                .tasks
                .iter()
                .filter(|(id, t)| {
                    !t.state.is_active()
                        && t.result.is_some()
                        && !self.results.get(id).is_some_and(|i| i.protected)
                })
                .map(|(id, _)| *id)
                .collect();
            let max_drop = if total_result_bytes > ordinary_ceiling {
                inactive_ids.len()
            } else {
                inactive_ids.len().saturating_sub(1)
            };
            for id in inactive_ids.into_iter().take(max_drop) {
                if total_result_bytes <= 256 * 1024 {
                    break;
                }
                if let Some(task) = self.tasks.get_mut(&id) {
                    if let Some(res) = task.result.take() {
                        total_result_bytes = total_result_bytes.saturating_sub(res.bytes());
                        let info = self.results.entry(id).or_default();
                        info.status = "expired".into();
                        info.reason = "ordinary cache capacity".into();
                    }
                }
            }
        }
    }

    pub(super) fn expire(&mut self) {
        let time = now();
        self.sessions
            .retain(|_, (_, _, at)| time.saturating_sub(*at) < 120);
        for t in self.tasks.values_mut() {
            if t.state == TaskState::Sent && time.saturating_sub(t.sent.unwrap_or(time)) >= 90 {
                t.state = TaskState::Unknown;
                let info = self.results.entry(t.id).or_default();
                info.completed_at = Some(time);
                info.status = "available".into();
                t.result = Some(error_result(
                    "response timeout: device execution is unknown; no automatic write retry",
                ));
                t.release_request();
            } else if t.state == TaskState::Queued && time.saturating_sub(t.created) >= 600 {
                t.state = TaskState::Expired;
                let info = self.results.entry(t.id).or_default();
                info.completed_at = Some(time);
                info.status = "available".into();
                t.result = Some(error_result("no device session within 600 seconds"));
                t.release_request();
            }
        }
        self.prune();
        self.release_terminal_dispatches();
    }

    pub(super) fn next(
        &mut self,
        device: &str,
        namespace: &str,
    ) -> io::Result<Option<transaction::PreparedRpc>> {
        if self
            .tasks
            .values()
            .any(|t| t.device == device && t.state == TaskState::Sent)
        {
            return Ok(None);
        }
        if !self.schedule_device(device) {
            return Ok(None);
        }
        let Some(task) = self
            .tasks
            .values()
            .find(|t| t.device == device && t.state == TaskState::Queued)
        else {
            return Ok(None);
        };
        let id = task.id;
        let write = super::task::is_write(&task.method);
        let epoch = task.args["_omci_auth_epoch"].as_u64();
        let internal_interval = self
            .interval_authorizations
            .get(&id)
            .is_some_and(|grant| grant.permits(task, self));
        let auth_valid = !write
            || crate::omci_auth::cwmp_epoch(device)
                .is_some_and(|current| current == 0 || epoch == Some(current));
        if let Err(reason) = if !auth_valid {
            Err("OMCI authentication generation changed; queued write cancelled".into())
        } else if internal_interval {
            super::policy::protect_unresolved(self, device, &task.method)
        } else {
            protect_management(self, device, &task.method, &task.args)
        } {
            self.fail_dispatch(id, &reason);
            return Ok(None);
        }

        let prepared = (|| {
            if let Some(args) = soap::WriteArgs::parse(&task.method, &task.args)? {
                let intent = super::intent::Intent::capture(&args)?;
                let xml = args.request(namespace, id, &intent.parameter_key);
                Ok((Some(intent), xml))
            } else {
                let xml = soap::request(namespace, id, &task.method, &task.args)?;
                Ok((None, xml))
            }
        })();
        let (intent, xml) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.release_dispatch(id);
                return Err(error);
            }
        };
        let task = self.tasks.get_mut(&id).unwrap();
        let undo = transaction::DispatchUndo {
            id,
            sent: task.sent,
        };
        let mut admit = || {
            task.state = TaskState::Sent;
            task.sent = Some(now());
            Ok(())
        };
        let admitted = if write {
            crate::omci_auth::enqueue_cwmp(device, epoch, admit)
        } else {
            admit()
        };
        if let Err(error) = admitted {
            self.fail_dispatch(id, &error.to_string());
            return Ok(None);
        }
        if let Some(intent) = intent {
            self.intents.insert(id, intent);
        }
        Ok(Some(transaction::PreparedRpc { xml, undo }))
    }

    fn fail_dispatch(&mut self, id: u64, reason: &str) {
        let task = self.tasks.get_mut(&id).unwrap();
        task.state = TaskState::Fault;
        task.result = Some(error_result(reason));
        self.mark_completed(id);
        self.release_dispatch(id);
    }

    pub(super) fn response(&mut self, device: &str, message: soap::Message) -> io::Result<()> {
        let id = message
            .id
            .strip_prefix("fttr-")
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "missing/mismatched CWMP ID")
            })?;
        let task = self
            .tasks
            .get_mut(&id)
            .filter(|t| t.device == device && t.state == TaskState::Sent)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unexpected CWMP response")
            })?;
        if message.method == "Fault" {
            task.state = TaskState::Fault;
        } else if message.method == format!("{}Response", task.method) {
            if matches!(
                task.method.as_str(),
                "SetParameterValues" | "AddObject" | "DeleteObject"
            ) && !matches!(message.data["status"].as_str(), Some("0" | "1"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SetParameterValues Status",
                ));
            }
            if task.method == "AddObject"
                && message.data["instance_number"]
                    .as_str()
                    .and_then(|s| s.parse::<u32>().ok())
                    .is_none_or(|n| n == 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid AddObject InstanceNumber",
                ));
            }
            task.state = if matches!(
                task.method.as_str(),
                "SetParameterValues" | "AddObject" | "DeleteObject"
            ) && message.data["status"] == "1"
            {
                TaskState::AcceptedPendingApply
            } else {
                TaskState::Completed
            };
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RPC response method mismatch",
            ));
        }
        let method = task.method.clone();
        let completed = task.state == TaskState::Completed;
        // If a successful write response cannot be retained, the consumer cannot
        // recover its status or AddObject instance. Do not turn that loss into
        // a definitive failure that could authorize a duplicate write.
        let lost_write_response_is_unknown =
            super::task::is_write(&method) && message.method != "Fault";
        if method == "GetParameterValues" && completed {
            let evidence = super::readback::service_evidence(&message.data);
            self.results.entry(id).or_default().management_services = evidence;
        }
        let value = message.data;
        let bytes = value.bytes();
        // Parser and dispatch both enforce this bound. Synthetic tests without a
        // dispatch lease still use the same global accounting.
        let permit = self.take_permit(id);
        let permit = match permit {
            Some(permit) => permit.shrink(bytes),
            None => self.budget.reserve(bytes, super::resources::RESULT_BYTES),
        };
        let result = permit.map(|p| super::resources::Response::charged(value, p));
        let task = self.tasks.get_mut(&id).unwrap();
        task.result = Some(match result {
            Some(result) => result,
            None => {
                task.state = if lost_write_response_is_unknown {
                    TaskState::Unknown
                } else {
                    TaskState::Fault
                };
                error_result(if lost_write_response_is_unknown {
                    "write response could not be retained; execution outcome requires recovery"
                } else {
                    "response_too_large: result exceeds its dispatch contract"
                })
            }
        });
        self.mark_completed(id);
        self.prune();
        Ok(())
    }
}
