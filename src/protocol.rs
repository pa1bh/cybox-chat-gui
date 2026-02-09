use chrono::TimeZone;
use chrono_tz::Europe::Amsterdam;
use serde::{Deserialize, Serialize};

pub fn format_uptime(seconds: u64) -> String {
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

pub fn format_at_prefix(at: Option<u64>) -> String {
    match at {
        Some(at_ms) => format!("[{}] ", format_unix_ms_nl_time(at_ms)),
        None => String::new(),
    }
}

fn format_unix_ms_nl_time(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let nanos = ((unix_ms % 1000) * 1_000_000) as u32;
    if let Some(dt) = Amsterdam.timestamp_opt(secs, nanos).single() {
        dt.format("%H:%M:%S").to_string()
    } else {
        "??:??:??".to_string()
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Outgoing {
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

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Incoming {
    #[serde(rename = "chat")]
    Chat {
        from: String,
        text: String,
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "system")]
    System {
        text: String,
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "ackName")]
    AckName {
        name: String,
        #[serde(default)]
        at: Option<u64>,
    },
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
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "listUsers")]
    ListUsers {
        users: Vec<UserInfo>,
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "pong")]
    Pong {
        token: Option<String>,
        #[serde(default)]
        at: Option<u64>,
    },
    #[serde(rename = "ai")]
    Ai {
        from: String,
        prompt: String,
        response: String,
        #[serde(rename = "responseMs")]
        response_ms: u64,
        tokens: Option<u32>,
        cost: Option<f64>,
        #[serde(default)]
        at: Option<u64>,
    },
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
}

pub enum ParsedInput {
    Empty,
    Error(String),
    Chat(String),
    SetName(String),
    Status,
    ListUsers,
    Ping(Option<String>),
    Ai(String),
}

pub fn parse_user_input(input: &str) -> ParsedInput {
    let text = input.trim();
    if text.is_empty() {
        return ParsedInput::Empty;
    }

    if !text.starts_with('/') {
        if text.chars().count() > 500 {
            return ParsedInput::Error("Message is too long (max 500 characters).".to_string());
        }
        return ParsedInput::Chat(text.to_string());
    }

    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd.as_str() {
        "/name" => {
            if arg.is_empty() {
                ParsedInput::Error("Usage: /name <new_name>".to_string())
            } else {
                let name_len = arg.chars().count();
                let name_valid = arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_');
                if !(2..=32).contains(&name_len) {
                    ParsedInput::Error("Naam moet tussen 2 en 32 tekens zijn.".to_string())
                } else if !name_valid {
                    ParsedInput::Error(
                        "Naam mag alleen letters, cijfers, spaties, - en _ bevatten.".to_string(),
                    )
                } else {
                    ParsedInput::SetName(arg.to_string())
                }
            }
        }
        "/status" => ParsedInput::Status,
        "/users" => ParsedInput::ListUsers,
        "/ping" => {
            if arg.is_empty() {
                ParsedInput::Ping(None)
            } else {
                ParsedInput::Ping(Some(arg.to_string()))
            }
        }
        "/ai" => {
            if arg.is_empty() {
                ParsedInput::Error("Usage: /ai <question>".to_string())
            } else if arg.chars().count() > 1000 {
                ParsedInput::Error("Vraag is te lang (max 1000 tekens).".to_string())
            } else {
                ParsedInput::Ai(arg.to_string())
            }
        }
        _ => ParsedInput::Error(format!("Unknown command: {}", cmd)),
    }
}

pub enum IncomingParse {
    Message(Incoming),
    Warning(String),
}

pub fn parse_incoming_text(text: &str) -> IncomingParse {
    if let Ok(incoming) = serde_json::from_str::<Incoming>(text) {
        return IncomingParse::Message(incoming);
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(msg_type) = value.get("type").and_then(|v| v.as_str()) {
            IncomingParse::Warning(format!("Unknown server message type: {}", msg_type))
        } else {
            IncomingParse::Warning("Server sent JSON without a valid 'type' field.".to_string())
        }
    } else {
        IncomingParse::Warning("Server sent invalid JSON.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_incoming_text, parse_user_input, Incoming, IncomingParse, ParsedInput};

    #[test]
    fn parse_name_command_validation() {
        let parsed = parse_user_input("/name !bad");
        assert!(matches!(parsed, ParsedInput::Error(_)));
    }

    #[test]
    fn parse_chat_too_long() {
        let long_text = "a".repeat(501);
        let parsed = parse_user_input(&long_text);
        assert!(matches!(parsed, ParsedInput::Error(_)));
    }

    #[test]
    fn parse_incoming_chat_with_at() {
        let json = r#"{"type":"chat","from":"Bas","text":"Hallo","at":1733312410000}"#;
        let parsed = parse_incoming_text(json);
        match parsed {
            IncomingParse::Message(Incoming::Chat { at, .. }) => {
                assert_eq!(at, Some(1733312410000));
            }
            _ => panic!("expected chat message"),
        }
    }

    #[test]
    fn parse_incoming_unknown_type_warning() {
        let json = r#"{"type":"newFeature","foo":"bar"}"#;
        let parsed = parse_incoming_text(json);
        match parsed {
            IncomingParse::Warning(text) => {
                assert!(text.contains("Unknown server message type"));
            }
            _ => panic!("expected warning"),
        }
    }

    #[test]
    fn format_at_prefix_is_human_readable() {
        let formatted = super::format_at_prefix(Some(1733312410000));
        assert!(formatted.starts_with('['));
        assert!(formatted.contains(":"));
        assert!(formatted.ends_with("] "));
    }
}
