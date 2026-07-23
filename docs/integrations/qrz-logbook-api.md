# QRZ Logbook API - Integration Reference

Source specification: <https://www.qrz.com/docs/logbook/QRZLogbookAPI.html>

This document is a comprehensive development reference for QsoRipper's consumption of the QRZ Logbook REST API for inserting, fetching, deleting, and managing QSO records.

---

## Overview

The QRZ Logbook API provides an HTTP REST interface for external programs to interact with QRZ Logbook data. It is a combination free and paid subscription service. Some advanced features (INSERT, DELETE, STATUS, FETCH) require a valid subscription. All QRZ members can access, edit, update, and view their complete logs through the QRZ website regardless of subscription status.

---

## Logbook data model

### Core concepts

- Every QSO record has a unique integer **`logid`**.
- Every logbook has a unique integer **`bookid`** and belongs to a specific QRZ member.
- A logbook serves exactly **one callsign**. Every character in a callsign is significant, including portable/mobile identifiers. For example, `XX1XX` and `XX1XX/M` are separate callsigns requiring separate logbooks.
- When a user changes callsigns, QRZ opens a new logbook. The user then has one logbook for each callsign.

### Required QSO fields for insertion

A QSO record requires these key attributes to be inserted:

1. The sending station's callsign (`station_callsign`)
2. The receiving station's callsign (`call`)
3. Date and time of the QSO (`qso_date`, `time_on`)
4. Frequency band (`band`)
5. Transmission mode (`mode`)

### Logbook date range

Each logbook has a configurable date range.
This range identifies when the callsign is active.
Usually, it starts on the license effective date and ends on the expiration date.
**QRZ rejects a QSO that has a date outside this range.**

### Access key model

- A QRZ member can have access to multiple logbooks.
- `bookid` values are never transmitted directly in the API.
- Instead, an opaque **API Access Key** provided by QRZ conveys both user identification and logbook routing.
- Get the access key for a logbook from the QRZ website.

---

## Endpoint and protocol

| Property | Value |
|---|---|
| Endpoint | `https://logbook.qrz.com/api` |
| Method | HTTP `POST` |
| Request body | URL-encoded `name=value` pairs |
| Response body | URL-encoded `name=value` pairs |
| QSO data format | ADIF, sent in the `ADIF` parameter |

---

## Application identification requirements

All applications **must** provide an identifiable `User-Agent` HTTP header.

**Format guidance from QRZ:**

- Personal scripts: include your callsign and a unique script name, for example `QsoRipper/0.1.0 (AA7BQ)`
- Applications: `ApplicationName/version`, for example `QsoRipper/1.0.0`
- Maximum length: **128 characters**

QRZ can limit applications that have missing or generic user agents, such as `node-fetch` and `python-requests`.

---

## Request parameters

Every API request must include `KEY` and `ACTION`. The server rejects requests containing unrecognized parameters.

### Request parameter types

| Parameter | Description |
|---|---|
| `KEY` | QRZ-supplied logbook access key |
| `ACTION` | Operation type: `INSERT`, `DELETE`, `STATUS`, `FETCH` |
| `ADIF` | ADIF-formatted QSO input data |
| `OPTION` | Action-specific options |
| `LOGIDS` | Comma-separated list of integer `logid` values |

### Response parameter types

| Parameter | Description |
|---|---|
| `RESULT` | `OK` on success, `FAIL` on failure, `AUTH` on insufficient privileges, or action-specific codes |
| `REASON` | Failure description (used with `RESULT=FAIL`) |
| `LOGIDS` | Comma-separated list of `logid` values affected by the action |
| `LOGID` | Single `logid` of inserted/replaced record (INSERT only, since it is a single-record operation) |
| `COUNT` | Number of QSO records affected by the action |
| `DATA` | Action-specific data payload (for example status reports) |

---

## API commands

### INSERT (subscription required)

Inserts one QSO record into the logbook selected by the API access key.

**Request:**

| Parameter | Value |
|---|---|
| `ACTION` | `INSERT` |
| `ADIF` | The ADIF data for insertion |
| `OPTION` | _(optional)_ `REPLACE` to automatically overwrite any existing duplicate QSOs |

**Response:**

| Parameter | Values |
|---|---|
| `RESULT` | `OK` (inserted), `FAIL` (not inserted), `REPLACE` (duplicate overwritten) |
| `COUNT` | Number of records inserted or replaced (always 1 or 0) |
| `LOGID` | The `logid` of the inserted or replaced record |
| `REASON` | Error description (when `RESULT=FAIL`) |

**Example request body (URL-encoded in practice):**

```
KEY=ABCD-0A0B-1C1D-2E2F&ACTION=INSERT&ADIF=<band:3>80m<mode:3>SSB<call:4>XX1X<qso_date:8>20140121<station_callsign:5>AA7BQ<time_on:4>0346<eor>
```

**Example response:**

```
RESULT=OK&LOGID=130877825&COUNT=1
```

**Implementation warnings:**

- The `REPLACE` option **will overwrite confirmed QSOs** with the supplied unconfirmed QSO data until QRZ re-verifies the match. Treat this as a high-risk operation requiring explicit user intent.
- Send the option as exactly `REPLACE`.
- Do not append a `LOGID` selector to the `OPTION` value.
- QRZ matches the duplicate from the supplied ADIF record and returns the affected `LOGID`.

---

### DELETE (subscription required)

Deletes one or more QSO records from the logbook selected by the API access key.

**Request:**

| Parameter | Value |
|---|---|
| `ACTION` | `DELETE` |
| `LOGIDS` | Comma-separated list of `logid` values to delete |

**Response:**

| Parameter | Values |
|---|---|
| `RESULT` | `OK` (all deleted), `PARTIAL` (some not found), `FAIL` (none found) |
| `LOGIDS` | Comma-separated list of `logid` values that were **not found** (only when `RESULT=PARTIAL`) |
| `COUNT` | Number of QSO records actually deleted |

**Critical warning:** This command **permanently deletes** records. There is **no undo**. You cannot recover deleted records. QsoRipper must get user confirmation before a DELETE operation.

---

### STATUS (subscription required)

Returns a status report for the logbook selected by the API access key.

**Request:**

| Parameter | Value |
|---|---|
| `ACTION` | `STATUS` |

**Response:**

| Parameter | Values |
|---|---|
| `RESULT` | `OK` (success), `FAIL` (invalid access key) |
| `DATA` | `&`-separated list of `name=value` pairs containing logbook status |

**The DATA payload can include:**

- Total QSOs in the logbook
- Total confirmed QSOs
- DXCC total
- USA states total
- Start and end dates
- Logbook owner
- `bookid`
- Logbook name
- Authorized users

---

### FETCH (subscription required)

Fetches one or more QSO records from the logbook matching specified criteria.

**Request:**

| Parameter | Value |
|---|---|
| `ACTION` | `FETCH` |
| `OPTION` | Comma-separated filter options (see below) |

**FETCH option parameters:**

Send options as a comma-separated list of colon-separated `name:value` pairs with **no spaces**. Example: `BAND:80m,MODE:SSB,MAX:400`

| Option | Description |
|---|---|
| `ALL` | Fetch the complete logbook (default). With this option, specify only `TYPE` and `STATUS`. |
| `DXCC:nnn` | Fetch records with DXCC=nnn |
| `BETWEEN:2014-01-01+2014-01-31` | Fetch records between start and end dates (inclusive) |
| `MODSINCE:2023-01-01` | Only return records modified since this date |
| `AFTERLOGID:123123123` | Only return records with `app_qrzlog_logid` >= the given value |
| `BAND:xxx` | Fetch QSOs on the given band |
| `MODE:xxx` | Fetch QSOs with the given mode |
| `CALL:XX1XX` | Fetch QSOs with the indicated callsign |
| `LOGIDS:nnn+nnn+nnn` | Fetch specific records by logid list (plus-separated) |
| `MAX:nnnn` | Maximum number of records to return (0 = count only. Unspecified = unlimited) |
| `TYPE:ADIF\|LOGIDS` | Response format: ADIF data (default) or logid list |
| `STATUS:CONFIRMED\|ALL` | Filter: confirmed records only, or all records (default: ALL) |

**Response:**

| Parameter | Values |
|---|---|
| `RESULT` | `OK` (matches found), `FAIL` (parameter or other problem) |
| `COUNT` | Total number of records matching selection criteria |
| `LOGIDS` | Comma-separated list of matching `logid` values (limited by `MAX`) |
| `ADIF` | ADIF data for matching QSOs (limited by `MAX`. Returned when `TYPE` is `ADIF` or default) |

**Usage notes:**

- You can combine multiple options. Separate them with `&` or `;`.
- `COUNT` always reflects the total match count for the given criteria regardless of `MAX`.
- To fetch **only the count**, set `MAX:0`.
- When you specify `ALL`, specify only `TYPE` and `STATUS` with it.

### Recommended paging strategy

Large logbooks can cause timeouts if fetched in one request. Use bounded fetches:

1. Start with `MAX:250,AFTERLOGID:0`
2. If QRZ returns 250 records, make another request.
3. Set `AFTERLOGID` to one more than the highest returned `app_qrzlog_logid`.
4. Repeat until QRZ returns fewer than 250 records or the specified `MAX`.

---

## Error handling policy for QsoRipper

| Scenario | Detection | QsoRipper behavior |
|---|---|---|
| Auth / privilege failure | `RESULT=AUTH` | Treat as credential/config issue. Do not retry. Surface to user |
| Validation failure | `RESULT=FAIL` with `REASON` | Surface clear reason to user. Keep local state unchanged |
| Partial delete | `RESULT=PARTIAL` | Log which logids were not found. Surface to user for review |
| Date range rejection | `RESULT=FAIL`, reason mentions date range | Surface as data validation error with the logbook's configured range |
| Network / transient failure | No response or HTTP error | Bounded retries with timeout and jitter |
| Rate limiting | HTTP 429 or similar | Back off with exponential delay |

**Core rule:** Never block local QSO logging on QRZ API availability. QRZ sync is an enrichment/upload path, not a prerequisite for local log operations.

---

## ADIF format notes

QRZ exchanges QSO data in ADIF (Amateur Data Interchange Format). Each field uses `<fieldname:length>value`. Each record ends with `<eor>`.

**Example ADIF record:**

```
<band:3>80m<mode:3>SSB<call:4>XX1X<qso_date:8>20140121<station_callsign:5>AA7BQ<time_on:4>0346<eor>
```

The QsoRipper adapter must include an ADIF parser and serializer that handles:

- Variable field lengths and ordering
- Optional fields present or absent per record
- Multi-record payloads from FETCH responses
- QRZ-compatible normalization for numeric fields on upload. For example,
  `TX_PWR` must be numeric watts. Normalize values such as `100W` to
  `100`. Omit values that you cannot parse.

---

## Mapping into QsoRipper domain

The adapter must parse ADIF and map it to internal QSO structures. It then sends normalized domain records to the application layer.

### Minimum field mapping

| ADIF field | QsoRipper domain |
|---|---|
| `station_callsign` | Local station identity |
| `call` | Worked callsign |
| `qso_date` + `time_on` | UTC timestamp (combined) |
| `band` | Band enum/value |
| `mode` | Mode enum/value |
| `rst_sent` | RST sent (optional) |
| `rst_rcvd` | RST received (optional) |
| `freq` | Frequency (optional) |
| `comment` / `notes` | Operator notes (optional) |
| `gridsquare` | Locator (optional) |

The adapter must preserve additional ADIF fields as extension data for a complete QRZ transfer cycle.

---

## Configuration keys

Use these environment variables (see `.env.example`):

| Variable | Purpose |
|---|---|
| `QSORIPPER_QRZ_LOGBOOK_BASE_URL` | Logbook API endpoint (default: `https://logbook.qrz.com/api`) |
| `QSORIPPER_QRZ_LOGBOOK_API_KEY` | QRZ-issued logbook access key |
| `QSORIPPER_QRZ_USER_AGENT` | User-Agent header value (for example `QsoRipper/0.1.0 (YOURCALL)`) |
| `QSORIPPER_QRZ_HTTP_TIMEOUT_SECONDS` | HTTP request timeout |
| `QSORIPPER_QRZ_MAX_RETRIES` | Maximum retry count for transient failures |
