# AGENTS.md

## Doel
Werk in deze repository als onderhoudbare Rust GUI-client voor de chatserver.

## Verplichte referentie
Bij protocol- of berichtwijzigingen moet je altijd afstemmen op:
- https://raw.githubusercontent.com/pa1bh/chatserver/refs/heads/main/REQUIREMENTS-CLIENTS.md

Zie bovenstaande specificatie als bron van waarheid voor transport, message types, velden en validaties.

## Projectcontext
- Stack: Rust + `eframe`/`egui` + `tokio-tungstenite`.
- Entry point: `src/main.rs`.
- Huidige scope: single-file app met UI, websocket-thread, message parsing en rendering.

## Werkregels voor agents
- Houd protocolcompatibiliteit met `REQUIREMENTS-CLIENTS.md` expliciet in stand.
- Voeg nieuwe message types toe in zowel:
  - `Outgoing` / `Incoming` enums
  - `send_message()` command routing
  - `process_incoming()` UI afhandeling
- Laat onbekende of optionele velden robuust toe (defensief parsen).
- Maak wijzigingen klein en toetsbaar; voorkom onnodige herstructurering.
- Gebruik duidelijke foutmeldingen in de UI (`ChatLine::Error`).

## Validatie na wijzigingen
Voer minimaal uit:

```bash
cargo check
```

Optioneel:

```bash
cargo run
```

Controleer handmatig:
- verbinden/verbreken;
- chatberichten;
- `/name`, `/status`, `/users`, `/ping`, `/ai`;
- correcte rendering van `error` events.

## Documentatie
Werk ook `rREADME.md` bij wanneer gedrag, commando's of protocolmapping verandert.
