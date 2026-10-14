//! Minimal QMP client: greeting, capabilities negotiation, commands with
//! ids, and events forwarded to a channel.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<Value, String>>;

#[derive(Clone)]
pub struct Qmp {
    requests: mpsc::UnboundedSender<(Value, Reply)>,
}

const MAX_LINE: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

impl Qmp {
    /// Negotiate on a connected monitor socket. `events` receives QMP events;
    /// `closed` fires once when the connection ends, with the reason.
    pub async fn connect<S>(
        stream: S,
        events: mpsc::UnboundedSender<Value>,
        closed: oneshot::Sender<String>,
    ) -> Result<Qmp, String>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = BufReader::new(reader);
        let greeting = read_line(&mut lines).await?.ok_or("QEMU closed the monitor before its greeting.")?;
        if greeting.get("QMP").is_none() {
            return Err("Unexpected QMP greeting.".into());
        }
        let (requests, mut queue) = mpsc::unbounded_channel::<(Value, Reply)>();
        // Lines are read on their own task: read_until is not cancel-safe in select!.
        let (incoming_tx, mut incoming) = mpsc::unbounded_channel::<Result<Option<Value>, String>>();
        tokio::spawn(async move {
            loop {
                let line = read_line(&mut lines).await;
                let end = !matches!(line, Ok(Some(_)));
                if incoming_tx.send(line).is_err() || end {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut pending: HashMap<u64, Reply> = HashMap::new();
            let mut next_id = 0u64;
            let reason = loop {
                tokio::select! {
                    request = queue.recv() => {
                        let Some((mut command, reply)) = request else { break "The monitor was closed.".to_string() };
                        next_id += 1;
                        command["id"] = json!(next_id);
                        let mut text = command.to_string();
                        text.push('\n');
                        if let Err(error) = writer.write_all(text.as_bytes()).await {
                            let _ = reply.send(Err(error.to_string()));
                            break format!("QEMU monitor write failed: {error}");
                        }
                        pending.insert(next_id, reply);
                    }
                    line = incoming.recv() => {
                        match line {
                            Some(Ok(Some(message))) => {
                                if message.get("event").is_some() {
                                    let _ = events.send(message);
                                } else if let Some(id) = message.get("id").and_then(Value::as_u64) {
                                    if let Some(reply) = pending.remove(&id) {
                                        let result = match message.get("error") {
                                            Some(error) => Err(error.get("desc").and_then(Value::as_str).unwrap_or("QMP error").to_string()),
                                            None => Ok(message.get("return").cloned().unwrap_or(Value::Null)),
                                        };
                                        let _ = reply.send(result);
                                    }
                                }
                            }
                            Some(Ok(None)) | None => break "QEMU closed the monitor.".to_string(),
                            Some(Err(error)) => break error,
                        }
                    }
                }
            };
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err(reason.clone()));
            }
            let _ = closed.send(reason);
        });
        let qmp = Qmp { requests };
        qmp.execute("qmp_capabilities", None).await?;
        Ok(qmp)
    }

    pub async fn execute(&self, name: &str, arguments: Option<Value>) -> Result<Value, String> {
        let mut command = json!({ "execute": name });
        if let Some(arguments) = arguments {
            command["arguments"] = arguments;
        }
        let (reply, result) = oneshot::channel();
        self.requests.send((command, reply)).map_err(|_| "The monitor was closed.".to_string())?;
        match tokio::time::timeout(TIMEOUT, result).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("The monitor was closed.".into()),
            Err(_) => Err(format!("QEMU did not answer {name} within {} seconds.", TIMEOUT.as_secs())),
        }
    }
}

async fn read_line<R: AsyncRead + Unpin>(lines: &mut BufReader<R>) -> Result<Option<Value>, String> {
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let count = (&mut *lines).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buffer).await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Ok(None);
        }
        if buffer.len() > MAX_LINE {
            return Err("QMP message too long.".into());
        }
        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        return serde_json::from_str(text).map(Some).map_err(|error| format!("Invalid QMP message: {error}"));
    }
}

