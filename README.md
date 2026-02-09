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
- Toont event timestamps (`at`) als lokale NL tijd (`HH:MM:SS`, `Europe/Amsterdam`).

## Installatie en draaien
Vereist: recente Rust toolchain (edition 2021).

```bash
cargo run
```

## Tests en controle

```bash
cargo check
cargo test
```

## Gebruik
- Typ gewone tekst om `chat` te versturen.
- Gebruik slash-commando's:
  - `/name <nieuwe_naam>`
  - `/status`
  - `/users`
  - `/ping [token]`
  - `/ai <vraag>`

Opmerking naamgedrag:
- Een naam gezet via `/name` wordt als voorkeurnaam opgeslagen.
- Bij een nieuwe connectie probeert de client die naam automatisch opnieuw te zetten.

## Persistente settings
De client bewaart instellingen lokaal:
- pad: `~/.config/cybox-chat-gui/settings.json`
- velden: `server_url`, `username` (voorkeurnaam)

Legacy fallback:
- Als aanwezig wordt oude `/.cybox-chat-gui-settings.json` in de projectmap nog gelezen.

## Structuur
- `src/main.rs`: GUI en eventverwerking.
- `src/network.rs`: WebSocket transportlaag en connectie-foutdiagnostiek.
- `src/protocol.rs`: protocolmodellen + input/incoming parsing + unit-tests.
- `src/settings.rs`: laden/opslaan van settings.
- `Cargo.toml`: dependencies en binary configuratie.

## Opmerkingen
- Houd compatibiliteit aan met de protocolspec:
  https://raw.githubusercontent.com/pa1bh/chatserver/refs/heads/main/REQUIREMENTS-CLIENTS.md
- Parser is defensief: onbekende/ongeldige serverberichten geven een zichtbare waarschuwing in de UI.
