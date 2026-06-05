# CatHub Native Frequency Probe

Native C++/Win32 diagnostic app for fast-launch CAT latency testing.

This is a no-.NET sibling of `experiments\cathub-frequency-probe`. It talks directly to cathub's rigctld-compatible endpoint at `127.0.0.1:4532` over Winsock, polls `f`, `m`, and `v` every 100 ms, and displays a large amber TS-590-style frequency readout.

It also loads `qsoripper_ffi.dll` beside the executable and uses the existing Rust FFI shim to call the engine gRPC `GetRigSnapshot` path at `http://127.0.0.1:50051`. That keeps the native probe aligned with the C# WinUI probe's direct cathub vs engine skew comparison without adding a C++ gRPC stack.

Build from the repository root:

```powershell
.\build.ps1 cathub-probe-native
```

The default `.\build.ps1` flow also builds this probe after Rust, .NET, and Win32 outputs. Running the standalone command expects `src\rust\target\release\qsoripper_ffi.dll` to already exist for engine skew; if it is missing, the direct cathub display still works but `ENGINE SKEW` reports `ERR`.

Run:

```powershell
artifacts\build\cathub-frequency-probe-native\Release\CatHubFrequencyProbeNative.exe
```

Diagnostic log:

```text
%LOCALAPPDATA%\qsoripper\cathub-frequency-probe-native.log
```

The `ENGINE SKEW` tile is `engine_frequency_hz - direct_cathub_frequency_hz`. `0 Hz` means the engine and cathub agree; a persistent non-zero value means the engine/UI path is behind or ahead of live cathub state.
