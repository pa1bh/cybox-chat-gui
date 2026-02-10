use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::sync::mpsc::UnboundedSender;

mod network;
mod protocol;
mod settings;

use network::{start_connection, SecurityInfo, UiEvent, WsCommand};
use protocol::{format_at_prefix, format_uptime, parse_user_input, Incoming, Outgoing, ParsedInput};
use settings::{load_settings, save_settings, AppSettings};

const AUTO_PING_INTERVAL_SECS: u64 = 5;
const MAX_LATENCY_SAMPLES: usize = 100;
const AUTO_PING_PREFIX: &str = "auto-";
const MAX_RAW_MESSAGES: usize = 500;

#[derive(Clone)]
struct RawLine {
    line: String,
    payload: String,
}

#[derive(Default, Clone)]
struct Metrics {
    ws_in_frames: u64,
    ws_out_frames: u64,
    reconnects: u64,
    connect_count: u64,
    last_connected_at: Option<Instant>,
    error_timestamps: VecDeque<Instant>,
}

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
    raw_messages: VecDeque<RawLine>,
    selected_raw_index: Option<usize>,
    connected: bool,
    preferred_username: String,
    username: String,

    // Channel to send messages to WebSocket
    ws_tx: Option<UnboundedSender<WsCommand>>,
    // Channel to receive events from WebSocket thread
    ui_rx: Option<Receiver<UiEvent>>,
    // Pending ping requests for roundtrip calculation
    pending_pings: HashMap<String, Instant>,
    latency_samples: VecDeque<f32>,
    last_auto_ping_sent: Option<Instant>,
    security_info: Option<SecurityInfo>,
    metrics: Metrics,
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
            raw_messages: VecDeque::new(),
            selected_raw_index: None,
            connected: false,
            preferred_username,
            username: settings.username,
            ws_tx: None,
            ui_rx: None,
            pending_pings: HashMap::new(),
            latency_samples: VecDeque::new(),
            last_auto_ping_sent: None,
            security_info: None,
            metrics: Metrics::default(),
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
                        if self.metrics.connect_count > 0 {
                            self.metrics.reconnects += 1;
                        }
                        self.metrics.connect_count += 1;
                        self.metrics.last_connected_at = Some(Instant::now());
                        self.connected = true;
                        self.last_auto_ping_sent = Some(Instant::now());
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
                        self.last_auto_ping_sent = None;
                        if let Some(reason) = reason {
                            self.messages.push(ChatLine::Error(reason));
                        }
                        self.messages.push(ChatLine::System {
                            text: "Disconnected".to_string(),
                            at: None,
                        });
                    }
                    UiEvent::Warning(text) => {
                        self.record_error_event();
                        self.messages.push(ChatLine::Error(text));
                    }
                    UiEvent::Error(text) => {
                        self.record_error_event();
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
                        let is_auto_ping = token
                            .as_ref()
                            .map(|t| t.starts_with(AUTO_PING_PREFIX))
                            .unwrap_or(false);
                        let token_str = token
                            .as_ref()
                            .map(|t| format!(" (token: {}...)", &t[..8.min(t.len())]))
                            .unwrap_or_default();
                        if let Some(rtt) = roundtrip {
                            let rtt_ms = (rtt.as_secs_f64() * 1000.0) as f32;
                            if is_auto_ping {
                                self.record_latency_sample(rtt_ms);
                            } else {
                                self.messages.push(ChatLine::Status {
                                    text: format!(
                                        "Pong! roundtrip: {:.2}ms{}",
                                        rtt.as_secs_f64() * 1000.0,
                                        token_str
                                    ),
                                    at,
                                });
                            }
                        } else {
                            if !is_auto_ping {
                                self.messages.push(ChatLine::Status {
                                    text: format!("Pong!{}", token_str),
                                    at,
                                });
                            }
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
                    UiEvent::Raw(line) => {
                        self.record_raw_line(line.clone());
                        if line.starts_with(">> ") {
                            self.metrics.ws_out_frames += 1;
                        } else if line.starts_with("<< ") {
                            self.metrics.ws_in_frames += 1;
                        }
                    }
                    UiEvent::Security(info) => {
                        self.security_info = Some(info);
                    }
                }
        }
    }

    fn record_error_event(&mut self) {
        let now = Instant::now();
        self.metrics.error_timestamps.push_back(now);
        self.prune_old_errors(now);
    }

    fn prune_old_errors(&mut self, now: Instant) {
        while let Some(oldest) = self.metrics.error_timestamps.front() {
            if now.duration_since(*oldest) > Duration::from_secs(60) {
                let _ = self.metrics.error_timestamps.pop_front();
            } else {
                break;
            }
        }
    }

    fn record_raw_line(&mut self, line: String) {
        let payload = line
            .strip_prefix(">> ")
            .or_else(|| line.strip_prefix("<< "))
            .unwrap_or(&line)
            .to_string();
        self.raw_messages.push_back(RawLine { line, payload });
        while self.raw_messages.len() > MAX_RAW_MESSAGES {
            let _ = self.raw_messages.pop_front();
            if let Some(sel) = self.selected_raw_index {
                self.selected_raw_index = sel.checked_sub(1);
            }
        }
    }

    fn record_latency_sample(&mut self, ms: f32) {
        self.latency_samples.push_back(ms);
        while self.latency_samples.len() > MAX_LATENCY_SAMPLES {
            let _ = self.latency_samples.pop_front();
        }
    }

    fn maybe_send_auto_ping(&mut self) {
        if !self.connected {
            return;
        }
        let now = Instant::now();
        let should_ping = self
            .last_auto_ping_sent
            .map(|last| now.duration_since(last).as_secs() >= AUTO_PING_INTERVAL_SECS)
            .unwrap_or(true);
        if !should_ping {
            return;
        }

        if let Some(tx) = &self.ws_tx {
            let token = format!("{}{}", AUTO_PING_PREFIX, uuid::Uuid::new_v4());
            self.pending_pings.insert(token.clone(), now);
            let _ = tx.send(WsCommand::Send(Outgoing::Ping { token: Some(token) }));
            self.last_auto_ping_sent = Some(now);
        }
    }

    fn draw_latency_graph(&self, ui: &mut egui::Ui, size: egui::Vec2) {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(20, 33, 47));
        painter.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(69, 101, 136)),
        );

        let inner = rect.shrink2(egui::vec2(8.0, 8.0));
        painter.text(
            egui::pos2(inner.left(), inner.top()),
            egui::Align2::LEFT_TOP,
            "Latency (ms)",
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(183, 214, 245),
        );

        if self.latency_samples.is_empty() {
            painter.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "Wachten op metingen...",
                egui::FontId::proportional(11.0),
                egui::Color32::from_gray(150),
            );
            return;
        }

        let chart_top = inner.top() + 16.0;
        let chart_bottom = inner.bottom() - 2.0;
        let chart_left = inner.left() + 2.0;
        let chart_right = inner.right() - 2.0;
        let max_value = self
            .latency_samples
            .iter()
            .copied()
            .fold(0.0_f32, f32::max)
            .max(20.0);

        for i in 0..=4 {
            let t = i as f32 / 4.0;
            let y = egui::lerp(chart_top..=chart_bottom, t);
            painter.line_segment(
                [egui::pos2(chart_left, y), egui::pos2(chart_right, y)],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 61, 84)),
            );
        }
        for i in 0..=5 {
            let t = i as f32 / 5.0;
            let x = egui::lerp(chart_left..=chart_right, t);
            painter.line_segment(
                [egui::pos2(x, chart_top), egui::pos2(x, chart_bottom)],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(35, 52, 73)),
            );
        }

        let mut points = Vec::with_capacity(self.latency_samples.len());
        let denom = (self.latency_samples.len().saturating_sub(1)).max(1) as f32;
        for (idx, value) in self.latency_samples.iter().enumerate() {
            let t = idx as f32 / denom;
            let x = egui::lerp(chart_left..=chart_right, t);
            let y = egui::remap_clamp(*value, 0.0..=max_value, chart_bottom..=chart_top);
            points.push(egui::pos2(x, y));
        }
        painter.line_segment(
            [egui::pos2(chart_left, chart_bottom), egui::pos2(chart_right, chart_bottom)],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(56, 81, 110)),
        );
        painter.add(egui::Shape::line(
            points.clone(),
            egui::Stroke::new(1.8, egui::Color32::from_rgb(111, 196, 255)),
        ));
        if let Some(last) = points.last() {
            painter.circle_filled(*last, 2.8, egui::Color32::from_rgb(157, 226, 255));
        }

        if let Some(last_ms) = self.latency_samples.back() {
            painter.text(
                egui::pos2(inner.right(), inner.top()),
                egui::Align2::RIGHT_TOP,
                format!("{:.1} ms", last_ms),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(157, 226, 255),
            );
        }
        painter.text(
            egui::pos2(inner.left(), chart_top),
            egui::Align2::LEFT_TOP,
            format!("{:.0}", max_value),
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(138),
        );
        painter.text(
            egui::pos2(inner.left(), chart_bottom),
            egui::Align2::LEFT_BOTTOM,
            "0",
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(138),
        );
    }

    fn latency_avg_ms(&self) -> Option<f32> {
        if self.latency_samples.is_empty() {
            return None;
        }
        let sum: f32 = self.latency_samples.iter().copied().sum();
        Some(sum / self.latency_samples.len() as f32)
    }

    fn latency_p95_ms(&self) -> Option<f32> {
        if self.latency_samples.is_empty() {
            return None;
        }
        let mut values = self.latency_samples.iter().copied().collect::<Vec<_>>();
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((values.len() as f32) * 0.95).ceil() as usize;
        values.get(idx.saturating_sub(1)).copied()
    }

    fn render_metrics_panel(&mut self, ui: &mut egui::Ui) {
        self.prune_old_errors(Instant::now());
        egui::CollapsingHeader::new("Metrics")
            .default_open(false)
            .show(ui, |ui| {
                let errors_per_min = self.metrics.error_timestamps.len();
                let avg = self
                    .latency_avg_ms()
                    .map(|v| format!("{:.1} ms", v))
                    .unwrap_or_else(|| "-".to_string());
                let p95 = self
                    .latency_p95_ms()
                    .map(|v| format!("{:.1} ms", v))
                    .unwrap_or_else(|| "-".to_string());

                let rows = vec![
                    ("Frames in", self.metrics.ws_in_frames.to_string()),
                    ("Frames out", self.metrics.ws_out_frames.to_string()),
                    ("Reconnects", self.metrics.reconnects.to_string()),
                    ("Avg latency", avg),
                    ("P95 latency", p95),
                    ("Errors/min", errors_per_min.to_string()),
                ];
                for (k, v) in rows {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [92.0, 16.0],
                            egui::Label::new(
                                egui::RichText::new(k)
                                    .small()
                                    .color(egui::Color32::from_gray(160)),
                            ),
                        );
                        ui.label(
                            egui::RichText::new(v)
                                .small()
                                .color(egui::Color32::from_rgb(190, 216, 244)),
                        );
                    });
                }
            });
    }

    fn render_security_panel(&self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Security / TLS")
            .default_open(false)
            .show(ui, |ui| {
                if let Some(info) = &self.security_info {
                    let rows = vec![
                        ("URL", info.url.clone()),
                        ("Transport", info.transport.clone()),
                        (
                            "TLS",
                            if info.tls {
                                "enabled".to_string()
                            } else {
                                "not enabled".to_string()
                            },
                        ),
                        (
                            "HTTP status",
                            info.http_status
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                        ),
                    ];
                    for (k, v) in rows {
                        ui.horizontal_wrapped(|ui| {
                            ui.add_sized(
                                [84.0, 16.0],
                                egui::Label::new(
                                    egui::RichText::new(k)
                                        .small()
                                        .color(egui::Color32::from_gray(160)),
                                ),
                            );
                            ui.label(
                                egui::RichText::new(v)
                                    .small()
                                    .color(egui::Color32::from_rgb(190, 216, 244)),
                            );
                        });
                    }
                    if !info.headers.is_empty() {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Handshake headers")
                                .small()
                                .strong()
                                .color(egui::Color32::from_rgb(164, 198, 233)),
                        );
                        for (k, v) in info.headers.iter().take(8) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{}:", k))
                                        .small()
                                        .color(egui::Color32::from_gray(160)),
                                );
                                ui.label(
                                    egui::RichText::new(v)
                                        .small()
                                        .monospace()
                                        .color(egui::Color32::from_rgb(156, 185, 215)),
                                );
                            });
                        }
                    }
                } else {
                    ui.label(
                        egui::RichText::new("Nog geen handshake info (nog niet verbonden).")
                            .small()
                            .color(egui::Color32::from_gray(160)),
                    );
                }
            });
    }

    fn render_json_value(ui: &mut egui::Ui, key: Option<&str>, value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                let title = key.unwrap_or("{object}");
                egui::CollapsingHeader::new(title).default_open(true).show(ui, |ui| {
                    for (k, v) in map {
                        Self::render_json_value(ui, Some(k), v);
                    }
                });
            }
            serde_json::Value::Array(arr) => {
                let title = key.unwrap_or("[array]");
                egui::CollapsingHeader::new(format!("{} [{}]", title, arr.len()))
                    .default_open(true)
                    .show(ui, |ui| {
                        for (idx, v) in arr.iter().enumerate() {
                            Self::render_json_value(ui, Some(&format!("[{}]", idx)), v);
                        }
                    });
            }
            _ => {
                let label = key.unwrap_or("value");
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(label)
                            .small()
                            .color(egui::Color32::from_gray(160)),
                    );
                    ui.label(
                        egui::RichText::new(value.to_string())
                            .small()
                            .monospace()
                            .color(egui::Color32::from_rgb(180, 212, 244)),
                    );
                });
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
        self.maybe_send_auto_ping();

        egui::TopBottomPanel::top("top_panel")
            .resizable(false)
            .show(ctx, |ui| {
                egui::Frame::default()
                    .fill(egui::Color32::from_rgb(24, 34, 48))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 83, 112)))
                    .rounding(egui::Rounding::same(10.0))
                    .outer_margin(egui::Margin::symmetric(6.0, 2.0))
                    .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                    .show(ui, |ui| {
                        let graph_h = 68.0;
                        let gap = 8.0;
                        let total_w = ui.available_width();
                        let graph_w = (total_w * 0.333).max(260.0);
                        let left_width = (total_w - graph_w - gap).max(220.0);
                        let graph_size = egui::vec2(graph_w, graph_h);

                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(left_width, graph_size.y),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Cybox Chat Client")
                                                .strong()
                                                .size(16.0)
                                                .color(egui::Color32::from_rgb(192, 218, 247)),
                                        );

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let (btn_text, btn_fill) = if self.connected {
                                                    ("Disconnect", egui::Color32::from_rgb(180, 70, 70))
                                                } else {
                                                    ("Connect", egui::Color32::from_rgb(45, 128, 86))
                                                };
                                                let btn = egui::Button::new(
                                                    egui::RichText::new(btn_text)
                                                        .strong()
                                                        .color(egui::Color32::WHITE),
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
                                                        self.last_auto_ping_sent = None;
                                                        self.messages.push(ChatLine::System {
                                                            text: "Disconnect requested".to_string(),
                                                            at: None,
                                                        });
                                                    } else {
                                                        self.connect(ctx.clone());
                                                    }
                                                }

                                                let (status_text, status_fill, status_stroke, status_dot) =
                                                    if self.connected {
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
                                                    .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                                                    .show(ui, |ui| {
                                                        ui.label(
                                                            egui::RichText::new(format!(
                                                                "● {}",
                                                                status_text
                                                            ))
                                                            .strong()
                                                            .color(status_dot),
                                                        );
                                                    });

                                                if !self.username.is_empty() {
                                                    egui::Frame::default()
                                                        .fill(egui::Color32::from_rgb(31, 44, 61))
                                                        .stroke(egui::Stroke::new(
                                                            1.0,
                                                            egui::Color32::from_rgb(75, 103, 136),
                                                        ))
                                                        .rounding(egui::Rounding::same(999.0))
                                                        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                                                        .show(ui, |ui| {
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "👤 {}",
                                                                    self.username
                                                                ))
                                                                .small()
                                                                .color(egui::Color32::from_rgb(
                                                                    169, 206, 246,
                                                                )),
                                                            );
                                                        });
                                                }
                                            },
                                        );
                                    });

                                    ui.add_space(3.0);
                                    ui.horizontal(|ui| {
                                        ui.add_sized(
                                            [42.0, 22.0],
                                            egui::Label::new(egui::RichText::new("Server").strong()),
                                        );
                                        let server_response = ui.add_sized(
                                            [ui.available_width() - 2.0, 22.0],
                                            egui::TextEdit::singleline(&mut self.server_url)
                                                .vertical_align(egui::Align::Center)
                                                .hint_text("ws://127.0.0.1:3001"),
                                        );
                                        if server_response.lost_focus() && server_response.changed() {
                                            self.persist_settings();
                                        }
                                    });
                                },
                            );

                            ui.add_space(gap);
                            self.draw_latency_graph(ui, graph_size);
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
                                    .vertical_align(egui::Align::Center)
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
                    let gap = 8.0;
                    let total_w = ui.available_width();
                    let left_w = ((total_w - gap) * 0.66).max(220.0);
                    let right_w = (total_w - gap - left_w).max(140.0);
                    let panel_h = ui.available_height();

                    ui.horizontal(|ui| {
                        ui.push_id("chat_pane", |ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(left_w, panel_h),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    egui::Frame::default()
                                        .fill(egui::Color32::from_rgba_unmultiplied(19, 26, 36, 210))
                                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(46, 63, 84)))
                                        .rounding(egui::Rounding::same(10.0))
                                        .inner_margin(egui::Margin::symmetric(8.0, 8.0))
                                        .show(ui, |ui| {
                                            egui::ScrollArea::vertical()
                                                .id_salt("chat_scroll")
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
                                                                egui::RichText::new(
                                                                    "Nog geen berichten. Verbind en start de chat.",
                                                                )
                                                                .italics()
                                                                .color(egui::Color32::from_gray(166)),
                                                            );
                                                        });
                                                    }
                                                });
                                        });
                                },
                            );
                        });

                        ui.add_space(gap);

                        ui.push_id("raw_pane", |ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(right_w, panel_h),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    egui::Frame::default()
                                        .fill(egui::Color32::from_rgba_unmultiplied(17, 23, 33, 220))
                                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 67, 90)))
                                        .rounding(egui::Rounding::same(10.0))
                                        .inner_margin(egui::Margin::symmetric(8.0, 8.0))
                                        .show(ui, |ui| {
                                            ui.label(
                                                egui::RichText::new("Raw WebSocket")
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(176, 209, 243)),
                                            );
                                            self.render_metrics_panel(ui);
                                            self.render_security_panel(ui);
                                            ui.separator();
                                            ui.label(
                                                egui::RichText::new("Frames")
                                                    .small()
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(164, 198, 233)),
                                            );
                                            let available_h = ui.available_height();
                                            let frames_h =
                                                (available_h * 0.65).clamp(220.0, 520.0);
                                            let inspector_h =
                                                (available_h * 0.28).clamp(110.0, 260.0);
                                            egui::ScrollArea::vertical()
                                                .id_salt("raw_scroll")
                                                .max_height(frames_h)
                                                .auto_shrink([false, false])
                                                .stick_to_bottom(true)
                                                .show(ui, |ui| {
                                                    for (idx, raw) in self.raw_messages.iter().enumerate() {
                                                        let selected = self.selected_raw_index == Some(idx);
                                                        if ui
                                                            .selectable_label(
                                                                selected,
                                                                egui::RichText::new(&raw.line)
                                                                    .monospace()
                                                                    .size(10.5)
                                                                    .color(egui::Color32::from_rgb(
                                                                        153, 181, 214,
                                                                    )),
                                                            )
                                                            .clicked()
                                                        {
                                                            self.selected_raw_index = Some(idx);
                                                        }
                                                    }
                                                });
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new("JSON Inspector")
                                                    .small()
                                                    .strong()
                                                    .color(egui::Color32::from_rgb(164, 198, 233)),
                                            );
                                            egui::ScrollArea::vertical()
                                                .id_salt("json_inspector_scroll")
                                                .max_height(inspector_h)
                                                .auto_shrink([false, false])
                                                .stick_to_bottom(false)
                                                .show(ui, |ui| {
                                                    if let Some(idx) = self.selected_raw_index {
                                                        if let Some(raw) = self.raw_messages.get(idx) {
                                                            match serde_json::from_str::<serde_json::Value>(
                                                                &raw.payload,
                                                            ) {
                                                                Ok(value) => {
                                                                    Self::render_json_value(
                                                                        ui,
                                                                        None,
                                                                        &value,
                                                                    );
                                                                }
                                                                Err(_) => {
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            "Geselecteerde regel is geen geldige JSON.",
                                                                        )
                                                                        .small()
                                                                        .color(egui::Color32::from_gray(160)),
                                                                    );
                                                                    ui.label(
                                                                        egui::RichText::new(
                                                                            &raw.payload,
                                                                        )
                                                                        .small()
                                                                        .monospace()
                                                                        .color(egui::Color32::from_rgb(
                                                                            153, 181, 214,
                                                                        )),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    } else {
                                                        ui.label(
                                                            egui::RichText::new(
                                                                "Selecteer een raw frame voor inspectie.",
                                                            )
                                                            .small()
                                                            .color(egui::Color32::from_gray(160)),
                                                        );
                                                    }
                                                });
                                        });
                                },
                            );
                        });
                    });
                });
        });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 980.0])
            .with_min_inner_size([820.0, 672.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Chat",
        options,
        Box::new(|_cc| Ok(Box::new(ChatApp::default()))),
    )
}
