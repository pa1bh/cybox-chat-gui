use std::io::ErrorKind;
use std::sync::mpsc::Sender;

use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::protocol::{parse_incoming_text, Incoming, IncomingParse, Outgoing};

#[derive(Debug, Clone)]
pub enum WsCommand {
    Send(Outgoing),
    Disconnect,
}

#[derive(Debug, Clone)]
pub struct SecurityInfo {
    pub url: String,
    pub transport: String,
    pub tls: bool,
    pub http_status: Option<u16>,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Connected,
    Disconnected(Option<String>),
    Incoming(Incoming),
    Raw(String),
    Security(SecurityInfo),
    Warning(String),
    Error(String),
}

pub fn start_connection(
    url: String,
    ui_tx: Sender<UiEvent>,
    ctx: egui::Context,
) -> UnboundedSender<WsCommand> {
    let (ws_tx, mut ws_rx) = unbounded_channel::<WsCommand>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws_stream, response)) => {
                    let transport = if url.to_ascii_lowercase().starts_with("wss://") {
                        "wss".to_string()
                    } else {
                        "ws".to_string()
                    };
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                v.to_str().unwrap_or("<non-utf8>").to_string(),
                            )
                        })
                        .collect::<Vec<_>>();
                    let _ = ui_tx.send(UiEvent::Security(SecurityInfo {
                        url: url.clone(),
                        transport: transport.clone(),
                        tls: transport == "wss",
                        http_status: Some(response.status().as_u16()),
                        headers,
                    }));
                    let _ = ui_tx.send(UiEvent::Connected);
                    ctx.request_repaint();

                    let (mut write, mut read) = ws_stream.split();
                    let ui_tx_write = ui_tx.clone();
                    let ctx_write = ctx.clone();
                    let write_handle = tokio::spawn(async move {
                        while let Some(cmd) = ws_rx.recv().await {
                            match cmd {
                                WsCommand::Send(msg) => {
                                    let json = serde_json::to_string(&msg).unwrap();
                                    let _ = ui_tx_write.send(UiEvent::Raw(format!(">> {}", json)));
                                    ctx_write.request_repaint();
                                    if write.send(Message::Text(json.into())).await.is_err() {
                                        break;
                                    }
                                }
                                WsCommand::Disconnect => {
                                    let _ = write.send(Message::Close(None)).await;
                                    break;
                                }
                            }
                        }
                    });

                    let mut emitted_disconnect = false;
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Text(text)) => {
                                let _ = ui_tx.send(UiEvent::Raw(format!("<< {}", text)));
                                match parse_incoming_text(&text) {
                                    IncomingParse::Message(incoming) => {
                                        let _ = ui_tx.send(UiEvent::Incoming(incoming));
                                        ctx.request_repaint();
                                    }
                                    IncomingParse::Warning(warning) => {
                                        let _ = ui_tx.send(UiEvent::Warning(warning));
                                        ctx.request_repaint();
                                    }
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Err(err) => {
                                emitted_disconnect = true;
                                let _ = ui_tx.send(UiEvent::Disconnected(Some(
                                    describe_stream_error(&err),
                                )));
                                ctx.request_repaint();
                                break;
                            }
                            _ => {}
                        }
                    }

                    write_handle.abort();
                    if !emitted_disconnect {
                        let _ = ui_tx.send(UiEvent::Disconnected(None));
                        ctx.request_repaint();
                    }
                }
                Err(err) => {
                    let _ = ui_tx.send(UiEvent::Error(describe_connect_error(&err)));
                    let _ = ui_tx.send(UiEvent::Disconnected(None));
                    ctx.request_repaint();
                }
            }
        });
    });

    ws_tx
}

fn describe_connect_error(err: &tungstenite::Error) -> String {
    match err {
        tungstenite::Error::Io(io_err) => match io_err.kind() {
            ErrorKind::ConnectionRefused => {
                "Connection refused. Controleer of de server draait en poort/open host klopt."
                    .to_string()
            }
            ErrorKind::TimedOut => {
                "Connection timed out. Controleer netwerk, host en firewall.".to_string()
            }
            ErrorKind::NotFound => {
                "DNS/host lookup failed. Controleer de server hostname.".to_string()
            }
            _ => format!("Network I/O error while connecting: {}", io_err),
        },
        tungstenite::Error::Tls(tls_err) => {
            format!("TLS handshake failed: {}", tls_err)
        }
        tungstenite::Error::Url(url_err) => {
            format!("Invalid WebSocket URL: {}", url_err)
        }
        _ => format!("Connection failed: {}", err),
    }
}

fn describe_stream_error(err: &tungstenite::Error) -> String {
    match err {
        tungstenite::Error::Io(io_err) => match io_err.kind() {
            ErrorKind::ConnectionReset => {
                "Connection reset by peer.".to_string()
            }
            ErrorKind::ConnectionAborted => {
                "Connection aborted.".to_string()
            }
            ErrorKind::TimedOut => "Connection timed out.".to_string(),
            _ => format!("Connection I/O error: {}", io_err),
        },
        _ => format!("Connection closed with error: {}", err),
    }
}
