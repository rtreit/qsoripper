# CatHub Frequency Probe

Single-purpose WinUI 3 diagnostic app for CAT latency testing.

The application does not use the normal QsoRipper UI update paths.
It opens one persistent TCP connection to the CatHub endpoint at `127.0.0.1:4532`.
This endpoint is compatible with `rigctld`.
The application polls `f`, `m`, and `v` every 100 ms.
It also polls the engine `GetRigSnapshot` endpoint at `http://127.0.0.1:50051`.

It displays the live direct cathub frequency, VFO, mode, query time, detected frequency-change gap, and engine skew. Engine skew is `engine_frequency_hz - direct_cathub_frequency_hz`.

Run from the repository root:

```powershell
dotnet run --project experiments\cathub-frequency-probe\CatHubFrequencyProbe.csproj
```

Publish the Native AOT executable from the repository root:

```powershell
dotnet publish experiments\cathub-frequency-probe\CatHubFrequencyProbe.csproj -c Release -p:PublishAot=true -o artifacts\publish\cathub-frequency-probe-aot\Release
```

Native AOT executable:

```text
artifacts\publish\cathub-frequency-probe-aot\Release\CatHubFrequencyProbe.exe
```

Diagnostic log:

```text
%LOCALAPPDATA%\qsoripper\cathub-frequency-probe.log
```

Interpretation:

- If direct cathub updates immediately and engine skew stays `0 Hz`, the slow behavior is in the individual UI poll/render/update path.
- If direct cathub updates immediately but engine skew diverges while tuning, the slow behavior is in the engine rig snapshot/cache/provider path.
- If direct cathub also lags, the problem is cathub state freshness or the radio/CAT path feeding cathub.
- Direct query time measures only cathub TCP response time, not the age of cathub's cached radio state.
