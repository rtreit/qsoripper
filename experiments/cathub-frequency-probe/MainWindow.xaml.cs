using System.Diagnostics;
using System.Globalization;
using System.Net.Sockets;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace CatHubFrequencyProbe;

public sealed partial class MainWindow : Window
{
    private static readonly TimeSpan PollInterval = TimeSpan.FromMilliseconds(100);
    private readonly CatHubRigClient _client;
    private readonly EngineRigClient _engineClient;
    private readonly DiagnosticLog _log = new();
    private readonly DispatcherQueueTimer _timer;
    private bool _pollInFlight;
    private ulong? _lastFrequencyHz;
    private DateTimeOffset? _lastFrequencyChange;
    private long _pollCount;

    public MainWindow()
    {
        InitializeComponent();

        Title = "CatHub Frequency Probe";
        ExtendsContentIntoTitleBar = false;
        _client = new CatHubRigClient("127.0.0.1", 4532);
        _engineClient = new EngineRigClient("http://127.0.0.1:50051");
        _timer = DispatcherQueue.CreateTimer();
        _timer.Interval = PollInterval;
        _timer.Tick += OnTimerTick;

        LogPathText.Text = $"log: {_log.Path}";
        WriteLog("probe starting; target=127.0.0.1:4532 poll=100ms commands=f,m,v");
        Closed += OnClosed;
        _timer.Start();
    }

    private async void OnTimerTick(DispatcherQueueTimer sender, object args)
    {
        if (_pollInFlight)
        {
            WriteLog("poll skipped; previous poll still in flight");
            return;
        }

        _pollInFlight = true;
        try
        {
            using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(1));
            var directSnapshot = await _client.ReadSnapshotAsync(timeout.Token);
            EngineFrequencySnapshot? engineSnapshot = null;
            string? engineError = null;
            try
            {
                engineSnapshot = await _engineClient.ReadSnapshotAsync(timeout.Token);
            }
            catch (Exception ex) when (ex is IOException or InvalidDataException or Grpc.Core.RpcException or OperationCanceledException)
            {
                engineError = $"{ex.GetType().Name}: {ex.Message}";
            }

            UpdateSnapshot(directSnapshot, engineSnapshot, engineError);
        }
        catch (Exception ex) when (ex is IOException or SocketException or InvalidOperationException or TimeoutException or OperationCanceledException)
        {
            FooterText.Text = "LINK DOWN - direct cathub rigctld read failed";
            WriteLog($"poll error: {ex.GetType().Name}: {ex.Message}");
        }
        finally
        {
            _pollInFlight = false;
        }
    }

    private void UpdateSnapshot(
        FrequencySnapshot snapshot,
        EngineFrequencySnapshot? engineSnapshot,
        string? engineError)
    {
        _pollCount++;
        var changed = _lastFrequencyHz != snapshot.FrequencyHz;
        long? changeGap = null;

        if (changed)
        {
            if (_lastFrequencyChange is { } previous)
            {
                changeGap = (long)(snapshot.SampledAt - previous).TotalMilliseconds;
            }

            _lastFrequencyHz = snapshot.FrequencyHz;
            _lastFrequencyChange = snapshot.SampledAt;
            WriteLog(string.Create(
                CultureInfo.InvariantCulture,
                $"change #{_pollCount}: direct={snapshot.FrequencyHz} Hz {FormatFrequency(snapshot.FrequencyHz)} MHz engine={EngineFrequencyForLog(engineSnapshot)} skew={FormatEngineSkew(snapshot, engineSnapshot)} vfo={snapshot.Vfo} mode={snapshot.Mode} direct_query={snapshot.QueryMilliseconds}ms engine_query={engineSnapshot?.QueryMilliseconds.ToString(CultureInfo.InvariantCulture) ?? "--"}ms gap={(changeGap?.ToString(CultureInfo.InvariantCulture) ?? "--")}ms"));
        }
        else if (_pollCount % 25 == 0)
        {
            WriteLog(string.Create(
                CultureInfo.InvariantCulture,
                $"poll #{_pollCount}: unchanged direct={snapshot.FrequencyHz} Hz engine={EngineFrequencyForLog(engineSnapshot)} skew={FormatEngineSkew(snapshot, engineSnapshot)} direct_query={snapshot.QueryMilliseconds}ms engine_query={engineSnapshot?.QueryMilliseconds.ToString(CultureInfo.InvariantCulture) ?? "--"}ms"));
        }

        if (engineError is not null && _pollCount % 10 == 1)
        {
            WriteLog($"engine poll error: {engineError}");
        }

        FrequencyText.Text = FormatFrequency(snapshot.FrequencyHz);
        VfoText.Text = snapshot.Vfo;
        ModeText.Text = snapshot.Mode;
        QueryText.Text = string.Create(CultureInfo.InvariantCulture, $"{snapshot.QueryMilliseconds} ms");
        ChangeGapText.Text = changeGap.HasValue
            ? string.Create(CultureInfo.InvariantCulture, $"{changeGap.Value} ms")
            : "-- ms";
        EngineSkewText.Text = engineError is not null
            ? "ERR"
            : FormatEngineSkew(snapshot, engineSnapshot);
        FooterText.Text = string.Create(
            CultureInfo.InvariantCulture,
            $"polls={_pollCount}  last={snapshot.SampledAt:HH:mm:ss.fff}  direct={FormatFrequency(snapshot.FrequencyHz)} MHz  engine={EngineFrequencyForFooter(engineSnapshot)}  skew={FormatEngineSkew(snapshot, engineSnapshot)}");
    }

    private void WriteLog(string message)
    {
        _log.Write(message);
        LogText.Text = _log.UiText;
        Debug.WriteLine(message);
    }

    private async void OnClosed(object sender, WindowEventArgs args)
    {
        _timer.Stop();
        await _client.DisposeAsync();
        _engineClient.Dispose();
    }

    private static string FormatFrequency(ulong frequencyHz)
    {
        var mhz = frequencyHz / 1_000_000;
        var khz = frequencyHz % 1_000_000 / 1_000;
        var hz = frequencyHz % 1_000;

        return $"{mhz}.{khz:000}.{hz:000}";
    }

    private static string FormatEngineSkew(FrequencySnapshot direct, EngineFrequencySnapshot? engine)
    {
        if (engine is null)
        {
            return "--";
        }

        var skew = (long)engine.FrequencyHz - (long)direct.FrequencyHz;
        return string.Create(CultureInfo.InvariantCulture, $"{skew:+#;-#;0} Hz");
    }

    private static string EngineFrequencyForLog(EngineFrequencySnapshot? engine)
    {
        return engine is null
            ? "--"
            : string.Create(CultureInfo.InvariantCulture, $"{engine.FrequencyHz} Hz {FormatFrequency(engine.FrequencyHz)} MHz");
    }

    private static string EngineFrequencyForFooter(EngineFrequencySnapshot? engine)
    {
        return engine is null
            ? "--"
            : string.Create(CultureInfo.InvariantCulture, $"{FormatFrequency(engine.FrequencyHz)} MHz/{engine.QueryMilliseconds}ms");
    }
}
