# QRZ XML Lookup API Integration Notes

Source reference: <https://www.qrz.com/docs/xml/current_spec.html>

This document captures how LogRipper should consume the QRZ XML data API for callsign and DXCC lookup enrichment.

## Endpoint and versioning

- Base endpoint: `https://xmldata.qrz.com/xml/`
- Preferred endpoint for latest spec: `https://xmldata.qrz.com/xml/current/`
- Versioned endpoints are supported (`/xml/1.34/`, etc.).
- If no version segment is provided, QRZ documents legacy behavior.

For LogRipper, use an explicit version policy (`current` or pinned version) in configuration to avoid accidental behavior drift.

## Auth and session lifecycle

Login request includes:

- `username`
- `password`
- `agent` (strongly recommended by QRZ)

Successful login returns `<Session><Key>...</Key>...</Session>`.

All follow-up requests must send that key as `s=<session-key>`.

Operational rules:

- Cache and reuse session keys until expiry/invalidity.
- Expect key invalidation and re-login when QRZ omits `<Key>` or returns session errors.
- Keep auth/session logic in the QRZ adapter, not UI/application layers.

## Response model

Top-level XML node: `<QRZDatabase>`

Common child nodes:

- `<Session>`
- `<Callsign>`
- `<DXCC>`

Parser requirement from QRZ spec: client decoders must tolerate unknown nodes/attributes and not assume field order.

## Core operations

### 1) Callsign lookup

Pattern:

- `s=<session-key>;callsign=<target>`

Returns `<Callsign>` fields plus `<Session>`.

### 2) DXCC lookup

Pattern:

- `dxcc=<number>` (entity by code)
- `dxcc=<callsign>` (entity match for callsign)
- `dxcc=all` (full list; use sparingly)

Returns `<DXCC>` plus `<Session>`.

### 3) Biography HTML fetch

Pattern:

- `html=<callsign>`

This is a special case: QRZ returns HTML content rather than XML.

## High-value callsign fields for normalization

Map provider fields into an internal `CallsignRecord` (or equivalent) model. Suggested subset:

- Identity: `call`, `aliases`, `xref`
- Operator: `fname`, `name`, `nickname`, `name_fmt`
- Geography: `country`, `dxcc`, `ccode`, `lat`, `lon`, `grid`, `cqzone`, `ituzone`
- QSL preferences: `eqsl`, `mqsl`, `lotw`, `qslmgr`
- Metadata: `image`, `url`, `moddate`, `geoloc`

Do not expose raw provider payloads above the integration boundary.

## Error handling

Use `<Session><Error>` as the primary error source and classify:

- Session/auth errors (for example timeout or invalid key) -> re-auth path
- Not-found errors -> negative cache entry with short TTL
- Service refusal or policy errors -> backoff and surface actionable status

When lookup fails, the QSO workflow must continue without blocking.

## Geolocation caveat

QRZ documents multiple `geoloc` sources (`user`, `geocode`, `grid`, `zip`, `state`, `dxcc`, `none`). Accuracy varies by source, so UI should treat location quality as variable metadata, not absolute truth.

## Configuration keys

Use these environment variables (see `.env.example`):

- `LOGRIPPER_QRZ_XML_BASE_URL`
- `LOGRIPPER_QRZ_XML_USERNAME`
- `LOGRIPPER_QRZ_XML_PASSWORD`
- `LOGRIPPER_QRZ_USER_AGENT`
- `LOGRIPPER_QRZ_HTTP_TIMEOUT_SECONDS`
- `LOGRIPPER_QRZ_MAX_RETRIES`

