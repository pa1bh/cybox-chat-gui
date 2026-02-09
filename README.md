# cybox-chat-gui

Grafische chatclient in Rust (`eframe`/`egui`) voor de WebSocket chatserver.

## Protocol
Deze client volgt de servercontracten uit:
- https://raw.githubusercontent.com/pa1bh/chatserver/refs/heads/main/REQUIREMENTS-CLIENTS.md

Gebruik bovenstaande specificatie als leidende bron voor berichttypes, velden en validatieregels.

## Wat deze client doet
- Verbindt via WebSocket met een chatserver (`ws://127.0.0.1:3001` standaard).
- Ondersteunt berichten en commando's:
  - `chat`
  - `setName` (`/name`)
  - `status` (`/status`)
  - `listUsers` (`/users`)
  - `ping` (`/ping`)
  - `ai` (`/ai`)
- Rendert inkomende serverberichten:
  - `chat`, `system`, `ackName`, `status`, `listUsers`, `error`, `pong`, `ai`

## Installatie en draaien
Vereist: recente Rust toolchain (edition 2021).

```bash
cargo run
```

De GUI start met een invoerveld voor server URL en connect/disconnect knop.

## Gebruik
- Typ gewone tekst om `chat` te versturen.
- Gebruik slash-commando's:
  - `/name <nieuwe_naam>`
  - `/status`
  - `/users`
  - `/ping [token]`
  - `/ai <vraag>`

## Structuur
- `src/main.rs`: volledige app (UI, websocket connectie, command parsing, rendering).
- `Cargo.toml`: dependencies en binary configuratie.

## Opmerkingen
- De server kan extra velden sturen (bijv. `at` timestamps); deze client parseert alleen de velden die hij gebruikt.
- Houd compatibiliteit aan met de protocolspec:
  https://raw.githubusercontent.com/pa1bh/chatserver/refs/heads/main/REQUIREMENTS-CLIENTS.md
