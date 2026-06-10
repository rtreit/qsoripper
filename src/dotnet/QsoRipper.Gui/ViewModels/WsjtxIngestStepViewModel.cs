using System.Collections.Generic;
using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using QsoRipper.Services;

namespace QsoRipper.Gui.ViewModels;

internal sealed partial class WsjtxIngestStepViewModel : WizardStepViewModel
{
    private const string DefaultUdpBind = "127.0.0.1:2237";
    private const uint DefaultPollIntervalMs = 1000;

    public override string Title => "WSJT-X ingestion (optional)";

    public override string Description =>
        "Import QSOs logged in WSJT-X using UDP packets, with optional ADIF tail recovery.";

    public override bool IsSkippable => true;

    [ObservableProperty]
    private bool _enabled;

    [ObservableProperty]
    private bool _udpEnabled = true;

    [ObservableProperty]
    private string _udpBind = DefaultUdpBind;

    [ObservableProperty]
    private bool _adifTailEnabled;

    [ObservableProperty]
    private string? _adifTailPath;

    [ObservableProperty]
    private int _pollIntervalMs = (int)DefaultPollIntervalMs;

    [ObservableProperty]
    private bool _syncToQrz;

    [ObservableProperty]
    private bool _hasExistingSettings;

    public override Dictionary<string, string> GetFields()
    {
        return new Dictionary<string, string>
        {
            ["enabled"] = Enabled.ToString(CultureInfo.InvariantCulture),
            ["udp_enabled"] = UdpEnabled.ToString(CultureInfo.InvariantCulture),
            ["udp_bind"] = UdpBind,
            ["adif_tail_enabled"] = AdifTailEnabled.ToString(CultureInfo.InvariantCulture),
            ["adif_tail_path"] = AdifTailPath ?? string.Empty,
            ["poll_interval_ms"] = PollIntervalMs.ToString(CultureInfo.InvariantCulture),
            ["sync_to_qrz"] = SyncToQrz.ToString(CultureInfo.InvariantCulture),
        };
    }

    public void ConfigureFromSettings(WsjtxIngestSettings? settings)
    {
        HasExistingSettings = settings is not null;
        Enabled = settings?.Enabled ?? false;
        UdpEnabled = settings?.UdpEnabled ?? true;
        UdpBind = string.IsNullOrWhiteSpace(settings?.UdpBind) ? DefaultUdpBind : settings.UdpBind;
        AdifTailEnabled = settings?.AdifTailEnabled ?? false;
        AdifTailPath = settings is { HasAdifTailPath: true } ? settings.AdifTailPath : string.Empty;
        PollIntervalMs = settings is { PollIntervalMs: > 0 }
            ? (int)settings.PollIntervalMs
            : (int)DefaultPollIntervalMs;
        SyncToQrz = settings?.SyncToQrz ?? false;
    }

    public bool ShouldSave =>
        HasExistingSettings
        || Enabled
        || !UdpEnabled
        || !string.Equals(UdpBind, DefaultUdpBind, StringComparison.Ordinal)
        || AdifTailEnabled
        || !string.IsNullOrWhiteSpace(AdifTailPath)
        || PollIntervalMs != DefaultPollIntervalMs
        || SyncToQrz;

    public WsjtxIngestSettings BuildSettings()
    {
        var settings = new WsjtxIngestSettings
        {
            Enabled = Enabled,
            UdpEnabled = UdpEnabled,
            UdpBind = string.IsNullOrWhiteSpace(UdpBind) ? DefaultUdpBind : UdpBind.Trim(),
            AdifTailEnabled = AdifTailEnabled,
            PollIntervalMs = PollIntervalMs > 0 ? (uint)PollIntervalMs : DefaultPollIntervalMs,
            SyncToQrz = SyncToQrz,
        };

        if (!string.IsNullOrWhiteSpace(AdifTailPath))
        {
            settings.AdifTailPath = AdifTailPath.Trim();
        }

        return settings;
    }

    public bool ValidateLocally()
    {
        if (UdpEnabled
            && (string.IsNullOrWhiteSpace(UdpBind) || !UdpBind.Contains(':', StringComparison.Ordinal)))
        {
            ValidationSummary = "UDP bind must be host:port (for example 127.0.0.1:2237).";
            return false;
        }

        if (AdifTailEnabled && string.IsNullOrWhiteSpace(AdifTailPath))
        {
            ValidationSummary = "ADIF tail path is required when ADIF tail recovery is enabled.";
            return false;
        }

        if (PollIntervalMs is < 100 or > 86_400_000)
        {
            ValidationSummary = "Poll interval must be between 100 and 86400000 milliseconds.";
            return false;
        }

        ClearErrors();
        return true;
    }
}
