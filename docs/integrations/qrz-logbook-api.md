# QRZ Logbook API Integration Notes

Source reference: <https://www.qrz.com/docs/logbook/QRZLogbookAPI.html>

This document captures how LogRipper should consume the QRZ Logbook API while staying aligned with our architecture rules (provider adapters at the edge, non-blocking UI, normalized internal models).

## Endpoint and protocol

- Endpoint: `https://logbook.qrz.com/api`
- Method: HTTP `POST`
- Body format: URL-encoded `name=value` pairs
- QSO payload format: ADIF in the `ADIF` parameter

## Required request shape

Every request must include:

- `KEY` - QRZ Logbook access key (routes to the right logbook)
- `ACTION` - operation name (for example: `INSERT`, `DELETE`, `STATUS`, `FETCH`)

All applications must send an identifiable `User-Agent` header. QRZ notes that missing or generic user agents can be rate-limited.

## Auth and identity model

- The logbook access key is the credential for this API surface.
- The key maps to a specific QRZ user/logbook context.
- Logbook identity details (`bookid`, owner, etc.) are server-managed and not sent directly in request parameters.

## Actions we should support first

### 1) INSERT (subscription required)

- Inserts one ADIF QSO record.
- Optional `OPTION=REPLACE` can overwrite an existing duplicate QSO.
- Response includes `RESULT`, `COUNT`, and `LOGID`.

Implementation note: treat `REPLACE` as a high-risk option because confirmed records may be overwritten.

### 2) DELETE (subscription required)

- Deletes one or more records by `LOGIDS`.
- Returns `OK`, `PARTIAL`, or `FAIL`.
- `PARTIAL` includes the logids not found.

### 3) STATUS (subscription required)

- Returns status metadata through `DATA` (for example totals, ownership, ranges, and book info).

### 4) FETCH (subscription required)

- Retrieves records by filter options (`BAND`, `MODE`, `CALL`, `BETWEEN`, `MODSINCE`, `DXCC`, `AFTERLOGID`, `MAX`, and others documented by QRZ).
- Can return either ADIF or `LOGIDS` list (`TYPE` option).
- `COUNT` is the total match count for criteria.

### Recommended paging strategy

Use bounded fetches such as `MAX:250,AFTERLOGID:0` and continue by increasing `AFTERLOGID` to `max_returned_logid + 1` until fewer than `MAX` rows are returned.

## Response semantics

Important response fields:

- `RESULT` - commonly `OK`, `FAIL`, `AUTH`, plus action-specific values such as `PARTIAL` and `REPLACE`
- `REASON` - failure details when `RESULT=FAIL`
- `COUNT` - affected or matched record count
- `LOGID`/`LOGIDS` - inserted or selected/deleted record IDs
- `DATA` - action-specific status payload

## Error handling policy

- `AUTH` or equivalent auth failures: treat as credential/config issues and stop retries.
- `FAIL` with validation issues: surface clear user-facing reason and keep local state unchanged.
- Network/transient failures: apply bounded retries with timeout and jitter.
- Never block local QSO logging flow on QRZ API availability.

## Mapping into LogRipper domain

The adapter should parse ADIF and map into internal QSO structures, then expose normalized domain records to the app layer.

Suggested minimum mapping:

- `station_callsign` -> local station identity
- `call` -> worked callsign
- `qso_date` + `time_on` -> UTC timestamp
- `band` -> band enum/value
- `mode` -> mode enum/value
- Optional exchange/report fields -> typed optional fields

## Configuration keys

Use these environment variables (see `.env.example`):

- `LOGRIPPER_QRZ_LOGBOOK_BASE_URL`
- `LOGRIPPER_QRZ_LOGBOOK_API_KEY`
- `LOGRIPPER_QRZ_USER_AGENT`
- `LOGRIPPER_QRZ_HTTP_TIMEOUT_SECONDS`
- `LOGRIPPER_QRZ_MAX_RETRIES`

