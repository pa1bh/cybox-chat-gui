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
    StatusCard {
        at: Option<u64>,
        rows: Vec<(String, String)>,
    },
    UsersCard {
        at: Option<u64>,
        users: Vec<(String, String, String)>,
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
    theme_initialized: bool,
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
            theme_initialized: false,
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
                        let mut rows = vec![
                            ("Version".to_string(), version),
                            ("Uptime".to_string(), format_uptime(uptime_seconds)),
                        ];

                        if let Some(os_name) = os {
                            rows.push((
                                "Platform".to_string(),
                                cpu_cores
                                    .map(|c| format!("{} ({} cores)", os_name, c))
                                    .unwrap_or(os_name),
                            ));
                        }
                        if let Some(rust_ver) = rust_version {
                            rows.push(("Rust".to_string(), rust_ver));
                        }
                        let users_value = if let Some(peak) = peak_users {
                            format!("{} (peak: {})", user_count, peak)
                        } else {
                            user_count.to_string()
                        };
                        rows.push(("Users".to_string(), users_value));
                        if let Some(conns) = connections_total {
                            rows.push(("Connections".to_string(), conns.to_string()));
                        }
                        rows.push(("Messages".to_string(), messages_sent.to_string()));
                        rows.push((
                            "Throughput".to_string(),
                            format!("{:.2} msg/s", messages_per_second),
                        ));
                        rows.push(("Memory".to_string(), format!("{:.2} MB", memory_mb)));
                        if let Some(enabled) = ai_enabled {
                            let ai_status = if enabled {
                                ai_model.unwrap_or_else(|| "enabled".to_string())
                            } else {
                                "disabled".to_string()
                            };
                            rows.push(("AI".to_string(), ai_status));
                        }
                        self.messages.push(ChatLine::StatusCard { at, rows });
                    }
                    UiEvent::Incoming(Incoming::ListUsers { users, at }) => {
                        let mapped = users
                            .into_iter()
                            .map(|u| (u.name, u.ip, u.id))
                            .collect::<Vec<_>>();
                        self.messages.push(ChatLine::UsersCard { at, users: mapped });
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

    fn apply_modern_theme(&mut self, ctx: &egui::Context) {
        if self.theme_initialized {
            return;
        }

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(9.0, 6.0);
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = egui::Color32::from_rgb(20, 26, 35);
        style.visuals.extreme_bg_color = egui::Color32::from_rgb(14, 19, 27);
        style.visuals.window_fill = egui::Color32::from_rgb(23, 30, 40);
        style.visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(52, 70, 92));
        style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(24, 31, 42);
        style.visuals.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(61, 80, 103));
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(27, 35, 48);
        style.visuals.widgets.inactive.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(71, 93, 118));
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(33, 48, 68);
        style.visuals.widgets.hovered.bg_stroke =
            egui::Stroke::new(1.0, egui::Color32::from_rgb(96, 137, 182));
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(62, 139, 217);
        style.visuals.override_text_color = Some(egui::Color32::from_rgb(223, 233, 247));
        ctx.set_style(style);

        self.theme_initialized = true;
    }

    fn render_chat_line(&self, ui: &mut egui::Ui, line: &ChatLine) {
        match line {
            ChatLine::Chat { from, text, at } => {
                let is_self = !self.username.is_empty() && from == &self.username;
                let fill = if is_self {
                    egui::Color32::from_rgb(23, 55, 83)
                } else {
                    egui::Color32::from_rgb(28, 35, 47)
                };
                let border = if is_self {
                    egui::Color32::from_rgb(58, 112, 153)
                } else {
                    egui::Color32::from_rgb(61, 75, 96)
                };
                egui::Frame::default()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0, border))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            let prefix = format_at_prefix(*at);
                            ui.label(
                                egui::RichText::new(format!("{}{}", prefix, from))
                                    .strong()
                                    .color(egui::Color32::from_rgb(149, 198, 241)),
                            );
                            ui.label(text);
                        });
                    });
            }
            ChatLine::System { text, at } => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(58, 51, 29))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(137, 121, 68)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        let prefix = format_at_prefix(*at);
                        ui.label(
                            egui::RichText::new(format!("{}{}", prefix, text))
                                .italics()
                                .color(egui::Color32::from_rgb(236, 214, 145)),
                        );
                    });
            }
            ChatLine::Error(text) => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(68, 33, 37))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(153, 73, 82)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(format!("✗ {}", text)).color(egui::Color32::from_rgb(246, 171, 171)));
                    });
            }
            ChatLine::Status { text, at } => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(31, 46, 67))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(83, 119, 161)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        let prefix = format_at_prefix(*at);
                        ui.label(egui::RichText::new(format!("{}{}", prefix, text)).color(egui::Color32::from_rgb(166, 204, 245)));
                    });
            }
            ChatLine::StatusCard { at, rows } => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(27, 40, 58))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(83, 119, 161)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        let prefix = format_at_prefix(*at);
                        ui.label(
                            egui::RichText::new(format!("{}Server status", prefix))
                                .strong()
                                .color(egui::Color32::from_rgb(182, 216, 249)),
                        );
                        ui.add_space(4.0);
                        for (label, value) in rows {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [110.0, 18.0],
                                    egui::Label::new(
                                        egui::RichText::new(label)
                                            .small()
                                            .color(egui::Color32::from_gray(178)),
                                    ),
                                );
                                ui.label(
                                    egui::RichText::new(value)
                                        .color(egui::Color32::from_rgb(214, 230, 248)),
                                );
                            });
                        }
                    });
            }
            ChatLine::UsersCard { at, users } => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(28, 43, 56))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(89, 126, 160)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        let prefix = format_at_prefix(*at);
                        ui.label(
                            egui::RichText::new(format!("{}Users ({})", prefix, users.len()))
                                .strong()
                                .color(egui::Color32::from_rgb(182, 216, 249)),
                        );
                        ui.add_space(4.0);
                        if users.is_empty() {
                            ui.label(
                                egui::RichText::new("No users connected")
                                    .color(egui::Color32::from_gray(180)),
                            );
                        } else {
                            for (name, ip, id) in users {
                                egui::Frame::default()
                                    .fill(egui::Color32::from_rgb(24, 36, 48))
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        egui::Color32::from_rgb(62, 86, 110),
                                    ))
                                    .rounding(egui::Rounding::same(6.0))
                                    .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(name)
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(208, 228, 250)),
                                            );
                                            ui.separator();
                                            ui.label(
                                                egui::RichText::new(ip)
                                                    .color(egui::Color32::from_gray(184)),
                                            );
                                        });
                                        ui.label(
                                            egui::RichText::new(format!("id: {}", id))
                                                .small()
                                                .color(egui::Color32::from_gray(146)),
                                        );
                                    });
                                ui.add_space(4.0);
                            }
                        }
                    });
            }
            ChatLine::Ai {
                from,
                prompt,
                response,
                stats,
                at,
            } => {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(23, 56, 50))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(73, 146, 128)))
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        let prefix = format_at_prefix(*at);
                        ui.label(
                            egui::RichText::new(format!("{}AI • {} vraagt: {}", prefix, from, prompt))
                                .strong()
                                .color(egui::Color32::from_rgb(130, 233, 198)),
                        );
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(response).color(egui::Color32::from_rgb(193, 235, 220)));
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(stats).small().color(egui::Color32::from_gray(164)));
                    });
            }
        }
    }
}

impl eframe::App for ChatApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_modern_theme(ctx);
        self.process_incoming();

        egui::TopBottomPanel::top("top_panel")
            .resizable(false)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(24, 34, 48))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 83, 112)))
                    .rounding(egui::Rounding::same(10.0))
                    .outer_margin(egui::Margin::symmetric(6.0, 4.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Cybox Chat Client")
                                    .strong()
                                    .size(17.0)
                                    .color(egui::Color32::from_rgb(192, 218, 247)),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let (btn_text, btn_fill) = if self.connected {
                                    ("Disconnect", egui::Color32::from_rgb(180, 70, 70))
                                } else {
                                    ("Connect", egui::Color32::from_rgb(45, 128, 86))
                                };
                                let btn = egui::Button::new(
                                    egui::RichText::new(btn_text).strong().color(egui::Color32::WHITE),
                                )
                                .fill(btn_fill)
                                .rounding(egui::Rounding::same(7.0))
                                .stroke(egui::Stroke::NONE);
                                if ui.add(btn).clicked() {
                                    if self.connected {
                                        if let Some(tx) = self.ws_tx.take() {
                                            let _ = tx.send(WsCommand::Disconnect);
                                        }
                                        self.connected = false;
                                        self.pending_pings.clear();
                                        self.messages.push(ChatLine::System {
                                            text: "Disconnect requested".to_string(),
                                            at: None,
                                        });
                                    } else {
                                        self.connect(ctx.clone());
                                    }
                                }

                                let (status_text, status_fill, status_stroke, status_dot) = if self.connected {
                                    (
                                        "Online",
                                        egui::Color32::from_rgb(33, 66, 48),
                                        egui::Color32::from_rgb(77, 138, 107),
                                        egui::Color32::from_rgb(104, 219, 152),
                                    )
                                } else {
                                    (
                                        "Offline",
                                        egui::Color32::from_rgb(73, 38, 42),
                                        egui::Color32::from_rgb(138, 84, 90),
                                        egui::Color32::from_rgb(240, 136, 136),
                                    )
                                };
                                egui::Frame::default()
                                    .fill(status_fill)
                                    .stroke(egui::Stroke::new(1.0, status_stroke))
                                    .rounding(egui::Rounding::same(999.0))
                                    .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new(format!("● {}", status_text))
                                                .strong()
                                                .color(status_dot),
                                        );
                                    });

                                if !self.username.is_empty() {
                                    egui::Frame::default()
                                        .fill(egui::Color32::from_rgb(31, 44, 61))
                                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(75, 103, 136)))
                                        .rounding(egui::Rounding::same(999.0))
                                        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new(format!("👤 {}", self.username))
                                                    .small()
                                                    .color(egui::Color32::from_rgb(169, 206, 246)),
                                            );
                                        });
                                }
                            });
                        });

                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [58.0, 26.0],
                                egui::Label::new(egui::RichText::new("Server").strong()),
                            );
                            let server_response = ui.add_sized(
                                [ui.available_width() - 2.0, 26.0],
                                egui::TextEdit::singleline(&mut self.server_url)
                                    .hint_text("ws://127.0.0.1:3001"),
                            );
                            if server_response.lost_focus() && server_response.changed() {
                                self.persist_settings();
                            }
                        });
                    });
        });

        egui::TopBottomPanel::bottom("input_panel")
            .resizable(false)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(24, 34, 48))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 83, 112)))
                    .rounding(egui::Rounding::same(10.0))
                    .outer_margin(egui::Margin::symmetric(6.0, 4.0))
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui.add_sized(
                                [ui.available_width() - 84.0, 26.0],
                                egui::TextEdit::singleline(&mut self.input)
                                    .hint_text("Type a message or /command..."),
                            );

                            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                self.send_message();
                                response.request_focus();
                            }

                            let send_btn = egui::Button::new(
                                egui::RichText::new("Send").strong().color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(48, 118, 194))
                            .rounding(egui::Rounding::same(7.0))
                            .stroke(egui::Stroke::NONE);
                            if ui.add(send_btn).clicked() {
                                self.send_message();
                            }
                        });

                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("Quick actions:").small());
                            for cmd in ["/status", "/users", "/ping"] {
                                let chip = egui::Button::new(egui::RichText::new(cmd).small())
                                    .rounding(egui::Rounding::same(999.0))
                                    .fill(egui::Color32::from_rgb(37, 50, 67));
                                if ui.add(chip).clicked() {
                                    self.input = cmd.to_string();
                                    self.send_message();
                                }
                            }
                            let ai_chip = egui::Button::new(egui::RichText::new("/ai ").small())
                                .rounding(egui::Rounding::same(999.0))
                                .fill(egui::Color32::from_rgb(33, 61, 54));
                            if ui.add(ai_chip).clicked() {
                                self.input = "/ai ".to_string();
                            }
                        });
                    });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(14, 19, 27));
            ui.painter().circle_filled(
                egui::pos2(rect.right() - 80.0, rect.top() + 45.0),
                140.0,
                egui::Color32::from_rgba_unmultiplied(52, 103, 166, 28),
            );
            ui.painter().circle_filled(
                egui::pos2(rect.left() + 40.0, rect.bottom() - 20.0),
                120.0,
                egui::Color32::from_rgba_unmultiplied(43, 130, 99, 24),
            );

            egui::Frame::default()
                .fill(egui::Color32::from_rgba_unmultiplied(23, 30, 40, 238))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(55, 74, 98)))
                .rounding(egui::Rounding::same(12.0))
                .inner_margin(egui::Margin::symmetric(10.0, 10.0))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &self.messages {
                                self.render_chat_line(ui, line);
                                ui.add_space(6.0);
                            }
                            if self.messages.is_empty() {
                                ui.add_space(12.0);
                                ui.centered_and_justified(|ui| {
                                    ui.label(
                                        egui::RichText::new("Nog geen berichten. Verbind en start de chat.")
                                            .italics()
                                            .color(egui::Color32::from_gray(166)),
                                    );
                                });
                            }
                        });
                });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Chat",
        options,
        Box::new(|_cc| Ok(Box::new(ChatApp::default()))),
    )
}
