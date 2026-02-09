use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::time::Instant;

use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;

mod network;
mod protocol;
mod settings;

use network::{start_connection, UiEvent, WsCommand};
use protocol::{format_at_prefix, format_uptime, parse_user_input, Incoming, Outgoing, ParsedInput};
use settings::{load_settings, save_settings, AppSettings};

fn is_guest_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().starts_with("guest-")
}

#[derive(Clone)]
enum ChatLine {
    Chat {
        from: String,
        text: String,
        at: Option<u64>,
    },
    System {
        text: String,
        at: Option<u64>,
    },
    Error(String),
    Status {
        text: String,
        at: Option<u64>,
    },
    Ai {
        from: String,
        prompt: String,
        response: String,
        stats: String,
        at: Option<u64>,
    },
}

struct ChatApp {
    server_url: String,
    input: String,
    messages: Vec<ChatLine>,
    connected: bool,
    preferred_username: String,
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
        let settings = load_settings();
        let preferred_username = if is_guest_name(&settings.username) {
            String::new()
        } else {
            settings.username.clone()
        };
        Self {
            server_url: settings.server_url,
            input: String::new(),
            messages: Vec::new(),
            connected: false,
            preferred_username,
            username: settings.username,
            ws_tx: None,
            ui_rx: None,
            pending_pings: HashMap::new(),
        }
    }
}

impl ChatApp {
    fn persist_settings(&mut self) {
        let settings = AppSettings {
            server_url: self.server_url.clone(),
            username: self.preferred_username.clone(),
        };

        if let Err(err) = save_settings(&settings) {
            self.messages.push(ChatLine::Error(err));
        }
    }

    fn connect(&mut self, ctx: egui::Context) {
        let url = self.server_url.clone();
        let (ui_tx, ui_rx) = channel::<UiEvent>();

        self.ws_tx = Some(start_connection(url, ui_tx, ctx));
        self.ui_rx = Some(ui_rx);
        self.persist_settings();
    }

    fn send_ws(&mut self, outgoing: Outgoing) {
        if let Some(tx) = &self.ws_tx {
            let _ = tx.send(WsCommand::Send(outgoing));
        } else {
            self.messages
                .push(ChatLine::Error("Not connected to server.".to_string()));
        }
    }

    fn send_message(&mut self) {
        let text = self.input.clone();
        match parse_user_input(&text) {
            ParsedInput::Empty => {}
            ParsedInput::Error(err) => self.messages.push(ChatLine::Error(err)),
            ParsedInput::Chat(text) => self.send_ws(Outgoing::Chat { text }),
            ParsedInput::SetName(name) => {
                self.preferred_username = name.clone();
                self.persist_settings();
                self.send_ws(Outgoing::SetName { name });
            }
            ParsedInput::Status => self.send_ws(Outgoing::Status),
            ParsedInput::ListUsers => self.send_ws(Outgoing::ListUsers),
            ParsedInput::Ping(token) => {
                let token = token.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                self.pending_pings.insert(token.clone(), Instant::now());
                self.send_ws(Outgoing::Ping { token: Some(token) });
            }
            ParsedInput::Ai(prompt) => {
                self.messages.push(ChatLine::System {
                    text: "AI is thinking...".to_string(),
                    at: None,
                });
                self.send_ws(Outgoing::Ai { prompt });
            }
        }

        self.input.clear();
    }

    fn process_incoming(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &self.ui_rx {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }

        for event in events {
            match event {
                    UiEvent::Connected => {
                        self.connected = true;
                        if !self.preferred_username.trim().is_empty()
                            && !is_guest_name(&self.preferred_username)
                        {
                            self.send_ws(Outgoing::SetName {
                                name: self.preferred_username.clone(),
                            });
                        }
                        self.messages.push(ChatLine::System {
                            text: "Connected!".to_string(),
                            at: None,
                        });
                    }
                    UiEvent::Disconnected(reason) => {
                        self.connected = false;
                        self.ws_tx = None;
                        self.pending_pings.clear();
                        if let Some(reason) = reason {
                            self.messages.push(ChatLine::Error(reason));
                        }
                        self.messages.push(ChatLine::System {
                            text: "Disconnected".to_string(),
                            at: None,
                        });
                    }
                    UiEvent::Warning(text) => {
                        self.messages.push(ChatLine::Error(text));
                    }
                    UiEvent::Error(text) => {
                        self.messages.push(ChatLine::Error(text));
                    }
                    UiEvent::Incoming(Incoming::Chat { from, text, at }) => {
                        self.messages.push(ChatLine::Chat { from, text, at });
                    }
                    UiEvent::Incoming(Incoming::System { text, at }) => {
                        self.messages.push(ChatLine::System { text, at });
                    }
                    UiEvent::Incoming(Incoming::AckName { name, at }) => {
                        self.username = name.clone();
                        self.messages.push(ChatLine::System {
                            text: format!("Your name is now: {}", name),
                            at,
                        });
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
                        at,
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
                            self.messages.push(ChatLine::Status { text: line, at });
                        }
                    }
                    UiEvent::Incoming(Incoming::ListUsers { users, at }) => {
                        if users.is_empty() {
                            self.messages.push(ChatLine::Status {
                                text: "No users connected".to_string(),
                                at,
                            });
                        } else {
                            self.messages.push(ChatLine::Status {
                                text: format!("Users ({})", users.len()),
                                at,
                            });
                            for u in &users {
                                self.messages.push(ChatLine::Status {
                                    text: format!("  {}  {}  {}", u.id, u.name, u.ip),
                                    at,
                                });
                            }
                        }
                    }
                    UiEvent::Incoming(Incoming::Error { message, at }) => {
                        let prefix = format_at_prefix(at);
                        self.messages.push(ChatLine::Error(format!("{}{}", prefix, message)));
                    }
                    UiEvent::Incoming(Incoming::Pong { token, at }) => {
                        let roundtrip = token
                            .as_ref()
                            .and_then(|t| self.pending_pings.remove(t).map(|start| start.elapsed()));
                        let token_str = token
                            .as_ref()
                            .map(|t| format!(" (token: {}...)", &t[..8.min(t.len())]))
                            .unwrap_or_default();
                        if let Some(rtt) = roundtrip {
                            self.messages.push(ChatLine::Status {
                                text: format!(
                                    "Pong! roundtrip: {:.2}ms{}",
                                    rtt.as_secs_f64() * 1000.0,
                                    token_str
                                ),
                                at,
                            });
                        } else {
                            self.messages.push(ChatLine::Status {
                                text: format!("Pong!{}", token_str),
                                at,
                            });
                        }
                    }
                    UiEvent::Incoming(Incoming::Ai {
                        from,
                        prompt,
                        response,
                        response_ms,
                        tokens,
                        cost,
                        at,
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
                            at,
                        });
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
                let server_response = ui.text_edit_singleline(&mut self.server_url);
                if server_response.lost_focus() && server_response.changed() {
                    self.persist_settings();
                }

                if self.connected {
                    if ui.button("Disconnect").clicked() {
                        if let Some(tx) = self.ws_tx.take() {
                            let _ = tx.send(WsCommand::Disconnect);
                        }
                        self.connected = false;
                        self.pending_pings.clear();
                        self.messages.push(ChatLine::System {
                            text: "Disconnect requested".to_string(),
                            at: None,
                        });
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
                            ChatLine::Chat { from, text, at } => {
                                ui.horizontal_wrapped(|ui| {
                                    let prefix = format_at_prefix(*at);
                                    ui.label(
                                        egui::RichText::new(format!("{}{}:", prefix, from)).strong(),
                                    );
                                    ui.label(text);
                                });
                            }
                            ChatLine::System { text, at } => {
                                let prefix = format_at_prefix(*at);
                                ui.label(
                                    egui::RichText::new(format!("{}* {}", prefix, text))
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
                            ChatLine::Status { text, at } => {
                                let prefix = format_at_prefix(*at);
                                ui.label(
                                    egui::RichText::new(format!("{}{}", prefix, text))
                                        .color(egui::Color32::LIGHT_BLUE),
                                );
                            }
                            ChatLine::Ai {
                                from,
                                prompt,
                                response,
                                stats,
                                at,
                            } => {
                                let prefix = format_at_prefix(*at);
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{}[AI] {} asked: {}",
                                            prefix, from, prompt
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
