# CatHub Native Frequency Probe

Native C++/Win32 diagnostic app for fast-launch CAT latency testing.

This is a no-.NET sibling of `experiments\cathub-frequency-probe`. It talks directly to cathub's rigctld-compatible endpoint at `127.0.0.1:4532` over Winsock, polls `f`, `m`, and `v` every 100 ms, and displays a large amber TS-590-style frequency readout.

Build from the repository root:

```powershell
cmake -S experiments\cathub-frequency-probe-native -B artifacts\build\cathub-frequency-probe-native -G "Visual Studio 18 2026" -A x64
cmake --build artifacts\build\cathub-frequency-probe-native --config Release
```

Run:

```powershell
artifacts\build\cathub-frequency-probe-native\Release\CatHubFrequencyProbeNative.exe
```

Diagnostic log:

```text
%LOCALAPPDATA%\qsoripper\cathub-frequency-probe-native.log
```

This native probe intentionally reads cathub directly and does not include the engine gRPC skew comparison from the C# WinUI probe. Its purpose is fast startup and direct cathub responsiveness checks.
