# CatHub Native Frequency Probe

Native C++/Win32 diagnostic app for fast-launch CAT latency testing.

This application is the native version of `experiments\cathub-frequency-probe`.
It uses Winsock to connect directly to CatHub at `127.0.0.1:4532`.
This endpoint is compatible with `rigctld`.
The application polls `f`, `m`, and `v` every 100 ms.
It shows the frequency in a large amber TS-590-style display.

It also loads `qsoripper_ffi.dll` beside the executable and uses the existing Rust FFI shim to call the engine gRPC `GetRigSnapshot` path at `http://127.0.0.1:50051`. That keeps the native probe aligned with the C# WinUI probe's direct cathub vs engine skew comparison without adding a C++ gRPC stack.

Build from the repository root:

```powershell
.\build.ps1 cathub-probe-native
```

The probe is optional.
The default `.\build.ps1` flow does not build it.
The standalone command uses `src\rust\target\release\qsoripper_ffi.dll` for engine skew.
If the DLL is absent, the direct CatHub display continues to work.
In this condition, `ENGINE SKEW` reports `ERR`.

Run:

```powershell
artifacts\build\cathub-frequency-probe-native\Release\CatHubFrequencyProbeNative.exe
```

Diagnostic log:

```text
%LOCALAPPDATA%\qsoripper\cathub-frequency-probe-native.log
```

The `ENGINE SKEW` tile is `engine_frequency_hz - direct_cathub_frequency_hz`. `0 Hz` means the engine and cathub agree. A persistent non-zero value means the engine/UI path is behind or ahead of live cathub state.
