use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;

fn format_uptime(seconds: u64) -> String {
    if seconds < 60 {
        format!("{} sec", seconds)
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else if seconds < 86400 {
        format!("{} uur", seconds / 3600)
    } else {
        format!("{} dagen", seconds / 86400)
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Outgoing {
    #[serde(rename = "chat")]
    Chat { text: String },
    #[serde(rename = "setName")]
    SetName { name: String },
    #[serde(rename = "status")]
    Status,
    #[serde(rename = "listUsers")]
    ListUsers,
    #[serde(rename = "ping")]
    Ping { token: Option<String> },
    #[serde(rename = "ai")]
    Ai { prompt: String },
}

#[derive(Debug, Clone)]
enum WsCommand {
    Send(Outgoing),
    Disconnect,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Incoming {
    #[serde(rename = "chat")]
    Chat { from: String, text: String },
    #[serde(rename = "system")]
    System { text: String },
    #[serde(rename = "ackName")]
    AckName { name: String },
    #[serde(rename = "status")]
    Status {
        version: String,
        #[serde(rename = "rustVersion")]
        rust_version: Option<String>,
        os: Option<String>,
        #[serde(rename = "cpuCores")]
        cpu_cores: Option<usize>,
        #[serde(rename = "uptimeSeconds")]
        uptime_seconds: u64,
        #[serde(rename = "userCount")]
        user_count: usize,
        #[serde(rename = "peakUsers")]
        peak_users: Option<usize>,
        #[serde(rename = "connectionsTotal")]
        connections_total: Option<u64>,
        #[serde(rename = "messagesSent")]
        messages_sent: u64,
        #[serde(rename = "messagesPerSecond")]
        messages_per_second: f64,
        #[serde(rename = "memoryMb")]
        memory_mb: f64,
        #[serde(rename = "aiEnabled")]
        ai_enabled: Option<bool>,
        #[serde(rename = "aiModel")]
        ai_model: Option<String>,
    },
    #[serde(rename = "listUsers")]
    ListUsers { users: Vec<UserInfo> },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "pong")]
    Pong { token: Option<String> },
    #[serde(rename = "ai")]
    Ai {
        from: String,
        prompt: String,
        response: String,
        #[serde(rename = "responseMs")]
        response_ms: u64,
        tokens: Option<u32>,
        cost: Option<f64>,
    },
}

#[derive(Debug, Clone)]
enum UiEvent {
    Connected,
    Disconnected(Option<String>),
    Incoming(Incoming),
    Warning(String),
    Error(String),
}

#[derive(Debug, Deserialize, Clone)]
struct UserInfo {
    id: String,
    name: String,
    ip: String,
}

#[derive(Clone)]
enum ChatLine {
    Chat {
        from: String,
        text: String,
    },
    System(String),
    Error(String),
    Status(String),
    Ai {
        from: String,
        prompt: String,
        response: String,
        stats: String,
    },
}

struct ChatApp {
    server_url: String,
    input: String,
    messages: Vec<ChatLine>,
    connected: bool,
    username: String,

    // Channel to send messages to WebSocket
    ws_tx: Option<UnboundedSender<WsCommand>>,
    // Channel to receive events from WebSocket thread
    ui_rx: Option<Receiver<UiEvent>>,
    // Pending ping requests for roundtrip calculation
    pending_pings: HashMap<String, Instant>,
}

impl Default for ChatApp {
    fn default() -> Self {
        Self {
            server_url: "ws://127.0.0.1:3001".to_string(),
            input: String::new(),
            messages: Vec::new(),
            connected: false,
            username: String::new(),
            ws_tx: None,
            ui_rx: None,
            pending_pings: HashMap::new(),
        }
    }
}

impl ChatApp {
    fn connect(&mut self, ctx: egui::Context) {
        let url = self.server_url.clone();

        // Channel for sending messages to WebSocket
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel::<WsCommand>();
        // Channel for receiving events from WebSocket thread
        let (ui_tx, ui_rx) = channel::<UiEvent>();

        self.ws_tx = Some(ws_tx);
        self.ui_rx = Some(ui_rx);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let result = tokio_tungstenite::connect_async(&url).await;

                match result {
                    Ok((ws_stream, _)) => {
                        let _ = ui_tx.send(UiEvent::Connected);
                        ctx.request_repaint();

                        let (mut write, mut read) = ws_stream.split();

                        // Spawn writer task
                        let write_handle = tokio::spawn(async move {
                            while let Some(cmd) = ws_rx.recv().await {
                                match cmd {
                                    WsCommand::Send(msg) => {
                                        let json = serde_json::to_string(&msg).unwrap();
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

                        // Read loop
                        while let Some(msg) = read.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    if let Ok(incoming) = serde_json::from_str::<Incoming>(&text) {
                                        let _ = ui_tx.send(UiEvent::Incoming(incoming));
                                        ctx.request_repaint();
                                    } else {
                                        let warning = if let Ok(value) =
                                            serde_json::from_str::<serde_json::Value>(&text)
                                        {
                                            if let Some(msg_type) =
                                                value.get("type").and_then(|v| v.as_str())
                                            {
                                                format!(
                                                    "Unknown server message type: {}",
                                                    msg_type
                                                )
                                            } else {
                                                "Server sent JSON without a valid 'type' field."
                                                    .to_string()
                                            }
                                        } else {
                                            "Server sent invalid JSON.".to_string()
                                        };
                                        let _ = ui_tx.send(UiEvent::Warning(warning));
                                        ctx.request_repaint();
                                    }
                                }
                                Ok(Message::Close(_)) => break,
                                Err(err) => {
                                    let _ = ui_tx.send(UiEvent::Disconnected(Some(format!(
                                        "Connection closed with error: {}",
                                        err
                                    ))));
                                    ctx.request_repaint();
                                    break;
                                }
                                _ => {}
                            }
                        }

                        write_handle.abort();
                        let _ = ui_tx.send(UiEvent::Disconnected(None));
                        ctx.request_repaint();
                    }
                    Err(err) => {
                        let _ = ui_tx.send(UiEvent::Error(format!(
                            "Connection failed: {}",
                            err
                        )));
                        let _ = ui_tx.send(UiEvent::Disconnected(None));
                        ctx.request_repaint();
                    }
                }
            });
        });
    }

    fn send_message(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        if !text.starts_with('/') && text.chars().count() > 500 {
            self.messages.push(ChatLine::Error(
                "Message is too long (max 500 characters).".to_string(),
            ));
            self.input.clear();
            return;
        }

        if let Some(tx) = &self.ws_tx {
            if text.starts_with('/') {
                let parts: Vec<&str> = text.splitn(2, ' ').collect();
                let cmd = parts[0].to_lowercase();
                let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

                match cmd.as_str() {
                    "/name" => {
                        if arg.is_empty() {
                            self.messages
                                .push(ChatLine::Error("Usage: /name <new_name>".to_string()));
                        } else {
                            let name_len = arg.chars().count();
                            let name_valid = arg.chars().all(|c| {
                                c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_'
                            });
                            if !(2..=32).contains(&name_len) {
                                self.messages.push(ChatLine::Error(
                                    "Naam moet tussen 2 en 32 tekens zijn.".to_string(),
                                ));
                            } else if !name_valid {
                                self.messages.push(ChatLine::Error(
                                    "Naam mag alleen letters, cijfers, spaties, - en _ bevatten."
                                        .to_string(),
                                ));
                            } else {
                                let _ = tx.send(WsCommand::Send(Outgoing::SetName {
                                    name: arg.to_string(),
                                }));
                            }
                        }
                    }
                    "/status" => {
                        let _ = tx.send(WsCommand::Send(Outgoing::Status));
                    }
                    "/users" => {
                        let _ = tx.send(WsCommand::Send(Outgoing::ListUsers));
                    }
                    "/ping" => {
                        let token = if arg.is_empty() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            arg.to_string()
                        };
                        self.pending_pings.insert(token.clone(), Instant::now());
                        let _ = tx.send(WsCommand::Send(Outgoing::Ping { token: Some(token) }));
                    }
                    "/ai" => {
                        if arg.is_empty() {
                            self.messages
                                .push(ChatLine::Error("Usage: /ai <question>".to_string()));
                        } else if arg.chars().count() > 1000 {
                            self.messages.push(ChatLine::Error(
                                "Vraag is te lang (max 1000 tekens).".to_string(),
                            ));
                        } else {
                            self.messages
                                .push(ChatLine::System("AI is thinking...".to_string()));
                            let _ = tx.send(WsCommand::Send(Outgoing::Ai {
                                prompt: arg.to_string(),
                            }));
                        }
                    }
                    _ => {
                        self.messages
                            .push(ChatLine::Error(format!("Unknown command: {}", cmd)));
                    }
                }
            } else {
                let _ = tx.send(WsCommand::Send(Outgoing::Chat { text }));
            }
        }

        self.input.clear();
    }

    fn process_incoming(&mut self) {
        if let Some(rx) = &self.ui_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    UiEvent::Connected => {
                        self.connected = true;
                        self.messages
                            .push(ChatLine::System("Connected!".to_string()));
                    }
                    UiEvent::Disconnected(reason) => {
                        self.connected = false;
                        self.ws_tx = None;
                        self.pending_pings.clear();
                        if let Some(reason) = reason {
                            self.messages.push(ChatLine::Error(reason));
                        }
                        self.messages
                            .push(ChatLine::System("Disconnected".to_string()));
                    }
                    UiEvent::Warning(text) => {
                        self.messages.push(ChatLine::Error(text));
                    }
                    UiEvent::Error(text) => {
                        self.messages.push(ChatLine::Error(text));
                    }
                    UiEvent::Incoming(Incoming::Chat { from, text }) => {
                        self.messages.push(ChatLine::Chat { from, text });
                    }
                    UiEvent::Incoming(Incoming::System { text }) => {
                        self.messages.push(ChatLine::System(text));
                    }
                    UiEvent::Incoming(Incoming::AckName { name }) => {
                        self.username = name.clone();
                        self.messages
                            .push(ChatLine::System(format!("Your name is now: {}", name)));
                    }
                    UiEvent::Incoming(Incoming::Status {
                        version,
                        rust_version,
                        os,
                        cpu_cores,
                        uptime_seconds,
                        user_count,
                        peak_users,
                        connections_total,
                        messages_sent,
                        messages_per_second,
                        memory_mb,
                        ai_enabled,
                        ai_model,
                    }) => {
                        let mut lines = vec![format!("Server Status v{}", version)];

                        if let Some(os_name) = os {
                            let cores = cpu_cores
                                .map(|c| format!(" ({} cores)", c))
                                .unwrap_or_default();
                            lines.push(format!("Platform: {}{}", os_name, cores));
                        }
                        if let Some(rust_ver) = rust_version {
                            lines.push(format!("Rust: {}", rust_ver));
                        }
                        lines.push(format!("Uptime: {}", format_uptime(uptime_seconds)));
                        let peak = peak_users
                            .map(|p| format!(" (peak: {})", p))
                            .unwrap_or_default();
                        lines.push(format!("Users: {}{}", user_count, peak));
                        if let Some(conns) = connections_total {
                            lines.push(format!("Connections: {}", conns));
                        }
                        lines.push(format!("Messages: {}", messages_sent));
                        lines.push(format!("Throughput: {} msg/s", messages_per_second));
                        lines.push(format!("Memory: {:.2} MB", memory_mb));
                        if let Some(enabled) = ai_enabled {
                            let ai_status = if enabled {
                                ai_model.unwrap_or_else(|| "enabled".to_string())
                            } else {
                                "disabled".to_string()
                            };
                            lines.push(format!("AI: {}", ai_status));
                        }

                        for line in lines {
                            self.messages.push(ChatLine::Status(line));
                        }
                    }
                    UiEvent::Incoming(Incoming::ListUsers { users }) => {
                        if users.is_empty() {
                            self.messages
                                .push(ChatLine::Status("No users connected".to_string()));
                        } else {
                            self.messages
                                .push(ChatLine::Status(format!("Users ({})", users.len())));
                            for u in &users {
                                self.messages.push(ChatLine::Status(format!(
                                    "  {}  {}  {}",
                                    u.id, u.name, u.ip
                                )));
                            }
                        }
                    }
                    UiEvent::Incoming(Incoming::Error { message }) => {
                        self.messages.push(ChatLine::Error(message));
                    }
                    UiEvent::Incoming(Incoming::Pong { token }) => {
                        let roundtrip = token.as_ref().and_then(|t| {
                            self.pending_pings.remove(t).map(|start| start.elapsed())
                        });
                        let token_str = token
                            .as_ref()
                            .map(|t| format!(" (token: {}...)", &t[..8.min(t.len())]))
                            .unwrap_or_default();
                        if let Some(rtt) = roundtrip {
                            self.messages.push(ChatLine::Status(format!(
                                "Pong! roundtrip: {:.2}ms{}",
                                rtt.as_secs_f64() * 1000.0,
                                token_str
                            )));
                        } else {
                            self.messages
                                .push(ChatLine::Status(format!("Pong!{}", token_str)));
                        }
                    }
                    UiEvent::Incoming(Incoming::Ai {
                        from,
                        prompt,
                        response,
                        response_ms,
                        tokens,
                        cost,
                    }) => {
                        let mut stats_parts = vec![format!("{}ms", response_ms)];
                        if let Some(t) = tokens {
                            stats_parts.push(format!("{} tokens", t));
                        }
                        if let Some(c) = cost {
                            stats_parts.push(format!("${:.4}", c));
                        }
                        self.messages.push(ChatLine::Ai {
                            from,
                            prompt,
                            response,
                            stats: stats_parts.join(" | "),
                        });
                    }
                }
            }
        }
    }
}

impl eframe::App for ChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_incoming();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Server:");
                ui.text_edit_singleline(&mut self.server_url);

                if self.connected {
                    if ui.button("Disconnect").clicked() {
                        if let Some(tx) = self.ws_tx.take() {
                            let _ = tx.send(WsCommand::Disconnect);
                        }
                        self.connected = false;
                        self.pending_pings.clear();
                        self.messages
                            .push(ChatLine::System("Disconnect requested".to_string()));
                    }
                    ui.label(egui::RichText::new("● Connected").color(egui::Color32::GREEN));
                    if !self.username.is_empty() {
                        ui.label(format!("({})", self.username));
                    }
                } else {
                    if ui.button("Connect").clicked() {
                        self.connect(ctx.clone());
                    }
                    ui.label(egui::RichText::new("● Disconnected").color(egui::Color32::RED));
                }
            });
        });

        egui::TopBottomPanel::bottom("input_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [ui.available_width() - 60.0, 24.0],
                    egui::TextEdit::singleline(&mut self.input)
                        .hint_text("Type a message or /command..."),
                );

                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.send_message();
                    response.request_focus();
                }

                if ui.button("Send").clicked() {
                    self.send_message();
                }
            });

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("/name /status /users /ping /ai")
                    .small()
                    .weak(),
            );
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.messages {
                        match line {
                            ChatLine::Chat { from, text } => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(egui::RichText::new(format!("{}:", from)).strong());
                                    ui.label(text);
                                });
                            }
                            ChatLine::System(text) => {
                                ui.label(
                                    egui::RichText::new(format!("* {}", text))
                                        .italics()
                                        .color(egui::Color32::YELLOW),
                                );
                            }
                            ChatLine::Error(text) => {
                                ui.label(
                                    egui::RichText::new(format!("✗ {}", text))
                                        .color(egui::Color32::RED),
                                );
                            }
                            ChatLine::Status(text) => {
                                ui.label(
                                    egui::RichText::new(text).color(egui::Color32::LIGHT_BLUE),
                                );
                            }
                            ChatLine::Ai {
                                from,
                                prompt,
                                response,
                                stats,
                            } => {
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "[AI] {} asked: {}",
                                            from, prompt
                                        ))
                                        .color(egui::Color32::from_rgb(180, 100, 255)),
                                    );
                                    ui.label(
                                        egui::RichText::new(response)
                                            .color(egui::Color32::LIGHT_BLUE),
                                    );
                                    ui.label(
                                        egui::RichText::new(stats)
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    );
                                });
                            }
                        }
                    }
                });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 450.0])
            .with_min_inner_size([400.0, 300.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Chat",
        options,
        Box::new(|_cc| Ok(Box::new(ChatApp::default()))),
    )
}
