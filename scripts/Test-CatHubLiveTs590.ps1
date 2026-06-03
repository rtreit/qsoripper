param(
    [string]$Port = "COM3",
    [int]$Baud = 115200,
    [switch]$Interactive
)

$ErrorActionPreference = "Stop"

$env:QSORIPPER_CATHUB_LIVE_TS590 = "1"
$env:QSORIPPER_CATHUB_LIVE_PORT = $Port
$env:QSORIPPER_CATHUB_LIVE_BAUD = [string]$Baud

if ($Interactive) {
    $env:QSORIPPER_CATHUB_LIVE_INTERACTIVE = "1"
} else {
    Remove-Item Env:\QSORIPPER_CATHUB_LIVE_INTERACTIVE -ErrorAction SilentlyContinue
}

Write-Host "Running live TS-590 cathub tests against $Port at $Baud baud."
Write-Host "Stop other CAT/serial clients before continuing. The current suite does not key PTT."

cargo test --manifest-path src\rust\Cargo.toml -p qsoripper-cathub --test live_ts590 -- --ignored --nocapture
exit $LASTEXITCODE
