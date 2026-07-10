using System.Collections.Generic;
using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using QsoRipper.Services;
using QsoRipper.Shared.Persistence;

namespace QsoRipper.Gui.ViewModels;

internal sealed partial class WsjtxIngestStepViewModel : WizardStepViewModel
{
    private bool _loading;

    [ObservableProperty]
    private bool _isDirty;

    public override string Title => "WSJT-X ingestion (optional)";

    public override string Description =>
        "Import QSOs logged in WSJT-X using UDP packets, with optional ADIF tail recovery.";

    public override bool IsSkippable => true;

    [ObservableProperty]
    private bool _enabled;

    [ObservableProperty]
    private bool _udpEnabled = true;

    [ObservableProperty]
    private string _udpBind = WsjtxIngestSetup.DefaultUdpBind;

    [ObservableProperty]
    private bool _adifTailEnabled;

    [ObservableProperty]
    private string? _adifTailPath;

    [ObservableProperty]
    private int _pollIntervalMs = (int)WsjtxIngestSetup.DefaultPollIntervalMs;

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
        _loading = true;
        try
        {
            HasExistingSettings = settings is not null;
            Enabled = settings?.Enabled ?? false;
            UdpEnabled = settings is { HasUdpEnabled: true } ? settings.UdpEnabled : true;
            UdpBind = string.IsNullOrWhiteSpace(settings?.UdpBind) ? WsjtxIngestSetup.DefaultUdpBind : settings.UdpBind;
            AdifTailEnabled = settings?.AdifTailEnabled ?? false;
            AdifTailPath = settings is { HasAdifTailPath: true } ? settings.AdifTailPath : string.Empty;
            PollIntervalMs = settings is { PollIntervalMs: > 0 }
                ? (int)settings.PollIntervalMs
                : (int)WsjtxIngestSetup.DefaultPollIntervalMs;
            SyncToQrz = settings?.SyncToQrz ?? false;
        }
        finally
        {
            _loading = false;
            IsDirty = false;
        }
    }

    public bool ShouldSave =>
        IsDirty
        || (!HasExistingSettings
            && (Enabled
                || !UdpEnabled
                || !string.Equals(UdpBind, WsjtxIngestSetup.DefaultUdpBind, StringComparison.Ordinal)
                || AdifTailEnabled
                || !string.IsNullOrWhiteSpace(AdifTailPath)
                || PollIntervalMs != WsjtxIngestSetup.DefaultPollIntervalMs
                || SyncToQrz));

    public WsjtxIngestSettings BuildSettings()
    {
        var settings = new WsjtxIngestSettings
        {
            Enabled = Enabled,
            UdpEnabled = UdpEnabled,
            UdpBind = string.IsNullOrWhiteSpace(UdpBind) ? WsjtxIngestSetup.DefaultUdpBind : UdpBind.Trim(),
            AdifTailEnabled = AdifTailEnabled,
            PollIntervalMs = PollIntervalMs > 0 ? (uint)PollIntervalMs : 0,
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
        if (!string.IsNullOrWhiteSpace(UdpBind)
            && !WsjtxIngestSetup.TryValidateHostPort(UdpBind, "UDP bind", out var hostPortError))
        {
            ValidationSummary = hostPortError;
            return false;
        }

        if (AdifTailEnabled && string.IsNullOrWhiteSpace(AdifTailPath))
        {
            ValidationSummary = "ADIF tail path is required when ADIF tail recovery is enabled.";
            return false;
        }

        if (PollIntervalMs < 0 || !WsjtxIngestSetup.IsValidPollInterval((uint)PollIntervalMs))
        {
            ValidationSummary = "Poll interval must be 0 for the engine default or a positive whole number.";
            return false;
        }

        ClearErrors();
        return true;
    }

    partial void OnEnabledChanged(bool value) => MarkDirty();
    partial void OnUdpEnabledChanged(bool value) => MarkDirty();
    partial void OnUdpBindChanged(string value) => MarkDirty();
    partial void OnAdifTailEnabledChanged(bool value) => MarkDirty();
    partial void OnAdifTailPathChanged(string? value) => MarkDirty();
    partial void OnPollIntervalMsChanged(int value) => MarkDirty();
    partial void OnSyncToQrzChanged(bool value) => MarkDirty();

    private void MarkDirty()
    {
        if (!_loading)
        {
            IsDirty = true;
        }
    }
}
