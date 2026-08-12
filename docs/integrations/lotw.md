# ARRL Logbook of the World

QsoRipper can upload QSOs to ARRL Logbook of the World (LoTW).
QsoRipper can also download LoTW confirmations.

## Requirements

Install TrustedQSL before you enable LoTW sync.
Use TrustedQSL to install your callsign certificate.
Use TrustedQSL to create a station location.
Make sure that TQSL can sign one test log.

## Configuration

Put non-secret settings in the shared `config.toml` file:

```toml
[lotw]
username = "KC7AVA"
tqsl_path = "C:\\Program Files (x86)\\TrustedQSL\\tqsl.exe"
station_location = "Home"
timeout_seconds = 60
```

Set the LoTW website password in `QSORIPPER_LOTW_PASSWORD`.
Set the certificate passphrase in `QSORIPPER_LOTW_CERTIFICATE_PASSWORD` when the certificate needs one.
Do not put either password in `config.toml`.

You can use these environment variables instead of the non-secret table values:

- `QSORIPPER_LOTW_USERNAME`
- `QSORIPPER_LOTW_TQSL_PATH`
- `QSORIPPER_LOTW_STATION_LOCATION`
- `QSORIPPER_LOTW_REPORT_URL`
- `QSORIPPER_LOTW_TIMEOUT_SECONDS`

Environment variables have priority over table values.

## Upload

The sync selects local-only, queued, modified, and failed LoTW records.
QsoRipper writes these records to a temporary ADIF file.
QsoRipper asks TQSL to sign and upload the file.
QsoRipper deletes the temporary file after TQSL stops.

A successful TQSL run changes each selected record to `UPLOADED`.
An upload failure changes each selected record to `FAILED`.
The local QSO stays in the logbook after a failure.

## Confirmation download

QsoRipper requests confirmed QSOs from the LoTW report service.
QsoRipper saves the returned `APP_LoTW_LASTQSL` value.
The next incremental sync requests only newer confirmations.
A full sync does not use the saved value.

QsoRipper matches a confirmation with these values:

- Station callsign
- Worked callsign
- Band
- Mode
- QSO time within 30 minutes

One match changes the local record to `CONFIRMED`.
More than one match changes all possible records to `CONFLICT`.
No match increases the unmatched count.

Confirmation updates keep local notes and comments.
They can add missing grid, DXCC, country, state, and county data.

QsoRipper exports a present false LoTW value as ADIF `N`.
This behavior keeps the correction for issue #14.

## Security

QsoRipper does not put LoTW passwords in TQSL output or sync errors.
QsoRipper does not put the report query in an error.
Use environment variables or a secure process configuration for secrets.

## Troubleshooting

Run TQSL directly when QsoRipper reports that TQSL did not start.
Confirm that the configured station location name is exact.
Confirm that the callsign certificate is valid.
Confirm that the LoTW website password is current.
Use a full sync when an incremental report does not contain an expected confirmation.
