//! Remote control: a local TCP port that lets `emergence-ctl` (or any script) drive the visible
//! sandbox and read back results.
//!
//! The protocol is one JSON object per line in each direction. Requests look like
//! `{"cmd":"step","ticks":10}`; replies look like `{"ok":true,"text":"...","data":{...}}`, where
//! `text` is a human-readable summary and `data` the structured result. Commands are run on the
//! UI thread between frames, so everything they do shows in the window. The port only listens
//! on 127.0.0.1.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use eframe::egui;
use serde::{Deserialize, Serialize};

/// Default port. Override with `EMERGENCE_CONTROL_PORT`; set it to 0 to turn control off.
pub(crate) const DEFAULT_PORT: u16 = 47_474;

/// Longest a command may take before the connection gives up (running every test can take a
/// while on a slow machine).
const REPLY_TIMEOUT: Duration = Duration::from_secs(600);

/// A command from a client.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub(crate) enum Request {
    Status,
    Scenarios,
    Tests,
    Load {
        name: String,
    },
    Watch {
        name: String,
    },
    Rebuild,
    Play,
    Pause,
    Speed {
        ticks_per_second: f32,
    },
    Step {
        ticks: u32,
    },
    Run {
        names: Vec<String>,
        wait: bool,
    },
    Results {
        failed_only: bool,
    },
    Open {
        node: Option<String>,
    },
    Up,
    Select {
        node: String,
    },
    Node {
        name: String,
    },
    Send {
        from: String,
        via: String,
        to: String,
        kind: String,
        data: String,
    },
    Logic {
        node: String,
        kind: String,
    },
    Limits {
        transmissions: Option<u64>,
        deliveries: Option<u64>,
        queue: Option<u64>,
        payload: Option<u64>,
    },
    Alerts {
        last: usize,
    },
    Log {
        last: usize,
    },
    Fuse,
    Templates,
    Add {
        template: String,
        name: String,
        parent: Option<String>,
        link: Option<String>,
        new_link: Option<String>,
        spec: Option<serde_json::Value>,
    },
    Remove {
        node: String,
    },
    Screenshot {
        path: String,
    },
    Quit,
}

/// A reply to a client.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Response {
    pub(crate) ok: bool,
    pub(crate) text: String,
    pub(crate) data: serde_json::Value,
}

impl Response {
    pub(crate) fn ok(text: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            text: text.into(),
            data,
        }
    }

    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self::ok(text, serde_json::Value::Null)
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: text.into(),
            data: serde_json::Value::Null,
        }
    }
}

/// A request waiting for the UI thread, with where to send the reply.
#[derive(Debug)]
pub(crate) struct Pending {
    pub(crate) request: Request,
    pub(crate) reply: Sender<Response>,
}

/// The running control server.
#[derive(Debug)]
pub(crate) struct ControlServer {
    pub(crate) requests: Receiver<Pending>,
    pub(crate) port: u16,
}

impl ControlServer {
    /// Starts listening on 127.0.0.1:`port` in the background. `ctx` is woken for each request
    /// so commands run even while nothing else is happening.
    pub(crate) fn start(port: u16, ctx: egui::Context) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (tx, ctx) = (tx.clone(), ctx.clone());
                    std::thread::spawn(move || serve(stream, &tx, &ctx));
                }
            })?;
        Ok(Self { requests: rx, port })
    }
}

fn serve(stream: TcpStream, tx: &Sender<Pending>, ctx: &egui::Context) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let (reply, answer) = channel();
                if tx.send(Pending { request, reply }).is_err() {
                    return;
                }
                ctx.request_repaint();
                answer
                    .recv_timeout(REPLY_TIMEOUT)
                    .unwrap_or_else(|_| Response::error("the sandbox did not answer in time"))
            }
            Err(e) => Response::error(format!("bad request: {e}")),
        };
        let Ok(json) = serde_json::to_string(&response) else {
            return;
        };
        if writeln!(writer, "{json}").is_err() {
            return;
        }
    }
}
