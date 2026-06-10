using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Collections.Specialized;
using System.ComponentModel;
using System.Globalization;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using QsoRipper.Domain;
using QsoRipper.Gui.Services;
using QsoRipper.Services;
using QsoRipper.Shared.Persistence;

namespace QsoRipper.Gui.ViewModels;

internal sealed partial class SettingsViewModel : ObservableObject
{
    private readonly IEngineClient _engine;

    // Station Profile
    [ObservableProperty]
    private string _callsign = string.Empty;

    [ObservableProperty]
    private string _gridSquare = string.Empty;

    [ObservableProperty]
    private string _operatorName = string.Empty;

    [ObservableProperty]
    private string _operatorCallsign = string.Empty;

    [ObservableProperty]
    private string _profileName = string.Empty;

    [ObservableProperty]
    private string _county = string.Empty;

    [ObservableProperty]
    private string _state = string.Empty;

    [ObservableProperty]
    private string _country = string.Empty;

    [ObservableProperty]
    private string _dxcc = string.Empty;

    [ObservableProperty]
    private string _cqZone = string.Empty;

    [ObservableProperty]
    private string _ituZone = string.Empty;

    [ObservableProperty]
    private string _latitude = string.Empty;

    [ObservableProperty]
    private string _longitude = string.Empty;

    [ObservableProperty]
    private string _arrlSection = string.Empty;

    // QRZ XML
    [ObservableProperty]
    private string _qrzXmlUsername = string.Empty;

    [ObservableProperty]
    private string _qrzXmlPassword = string.Empty;

    [ObservableProperty]
    private bool _isTestingQrzXml;

    [ObservableProperty]
    private string? _qrzXmlTestResult;

    [ObservableProperty]
    private bool _qrzXmlTestSucceeded;

    // QRZ Logbook
    [ObservableProperty]
    private string _qrzLogbookApiKey = string.Empty;

    [ObservableProperty]
    private bool _isTestingLogbook;

    [ObservableProperty]
    private string? _logbookTestResult;

    [ObservableProperty]
    private bool _logbookTestSucceeded;

    // Sync Settings
    [ObservableProperty]
    private bool _autoSyncEnabled;

    [ObservableProperty]
    private int _syncIntervalSeconds = 300;

    [ObservableProperty]
    private ConflictPolicy _conflictPolicy = ConflictPolicy.LastWriteWins;

    // Rig control
    [ObservableProperty]
    private bool _rigControlEnabled;

    [ObservableProperty]
    private string _rigControlHost = string.Empty;

    [ObservableProperty]
    private string _rigControlPort = string.Empty;

    [ObservableProperty]
    private string _rigControlReadTimeoutMs = string.Empty;

    [ObservableProperty]
    private string _rigControlStaleThresholdMs = string.Empty;

    // CAT hub ([cat_hub]) — the engine owns this whole section. The editor below
    // only emits it on save when the operator actually edits it (IsCatHubDirty),
    // so an untouched section keeps its comments and unknown keys verbatim.
    [ObservableProperty]
    private string _catHubBackend = string.Empty;

    [ObservableProperty]
    private string _catHubModel = string.Empty;

    [ObservableProperty]
    private string _catHubTransport = string.Empty;

    [ObservableProperty]
    private string _catHubPort = string.Empty;

    [ObservableProperty]
    private string _catHubBaud = string.Empty;

    [ObservableProperty]
    private string _catHubHost = string.Empty;

    [ObservableProperty]
    private string _catHubTcpPort = string.Empty;

    // Tri-state ComboBox index: 0 = daemon default (omit), 1 = yes, 2 = no.
    [ObservableProperty]
    private int _catHubCertifiedIndex;

    [ObservableProperty]
    private string _catHubReplyTimeoutMs = string.Empty;

    [ObservableProperty]
    private string _catHubPollBaselineMs = string.Empty;

    [ObservableProperty]
    private string _catHubPollHeartbeatMs = string.Empty;

    [ObservableProperty]
    private string _catHubPttMaxTxMs = string.Empty;

    // Tri-state ComboBox index: 0 = daemon default (omit), 1 = on, 2 = off.
    [ObservableProperty]
    private int _catHubNativePushIndex;

    private bool _hasPersistedCatHub;
    private bool _catHubLoading;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowCatHubRewriteWarning))]
    private bool _isCatHubDirty;

    /// <summary>
    /// True once the operator edits a CAT hub field for a section that already
    /// exists on disk. Surfacing this warns that saving rewrites the whole
    /// <c>[cat_hub]</c> section and drops comments / unknown keys.
    /// </summary>
    public bool ShowCatHubRewriteWarning => IsCatHubDirty && _hasPersistedCatHub;

    public ObservableCollection<CatHubFaceRowViewModel> CatHubFaces { get; } = [];

    public ObservableCollection<CatHubEndpointRowViewModel> CatHubEndpoints { get; } = [];

    // WSJT-X ingestion ([wsjtx_ingest]) is conditionally engine-owned like CAT hub.
    // Untouched settings are omitted on save so existing comments and unknown keys stay intact.
    [ObservableProperty]
    private bool _wsjtxIngestEnabled;

    [ObservableProperty]
    private bool _wsjtxUdpEnabled = true;

    [ObservableProperty]
    private string _wsjtxUdpBind = "127.0.0.1:2237";

    [ObservableProperty]
    private bool _wsjtxAdifTailEnabled;

    [ObservableProperty]
    private string _wsjtxAdifTailPath = string.Empty;

    [ObservableProperty]
    private string _wsjtxPollIntervalMs = "1000";

    [ObservableProperty]
    private bool _wsjtxSyncToQrz;

    private bool _hasPersistedWsjtxIngest;
    private bool _wsjtxIngestLoading;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowWsjtxIngestRewriteWarning))]
    private bool _isWsjtxIngestDirty;

    public bool ShowWsjtxIngestRewriteWarning => IsWsjtxIngestDirty && _hasPersistedWsjtxIngest;

    [ObservableProperty]
    private string _persistenceDescription = "Where should QsoRipper store persisted logbook data?";

    [ObservableProperty]
    private string _persistenceSectionTitle = "Storage";

    [ObservableProperty]
    private bool _isSpaceWeatherVisible;

    // Radio monitor (round 1: GUI-side cw-decoder host driving CW WPM auto-fill)
    [ObservableProperty]
    private bool _isRadioMonitorEnabled;

    [ObservableProperty]
    private bool _isCwWpmStatusBarVisible;

    /// <summary>
    /// When true, every cw-decoder NDJSON event + audio is mirrored to a
    /// per-session diagnostics directory. See
    /// <see cref="QsoRipper.Gui.Services.CwDiagnosticsRecorder"/> for the
    /// on-disk layout.
    /// </summary>
    [ObservableProperty]
    private bool _isAdvancedDiagnosticsEnabled;

    public ObservableCollection<RadioMonitorDevice> RadioMonitorDevices { get; } = [];

    [ObservableProperty]
    private RadioMonitorDevice? _selectedRadioMonitorDevice;

    [ObservableProperty]
    private string _radioMonitorBinaryStatus = string.Empty;

    [ObservableProperty]
    private bool _isLoadingRadioMonitorDevices;

    /// <summary>
    /// Captured by <see cref="MainWindowViewModel.CreateSettingsViewModel"/> so
    /// <see cref="RefreshRadioMonitorDevicesAsync"/> can preselect the persisted
    /// device once the device list is populated. Avoids the race where the
    /// catalog auto-selects "System default" before the caller can apply its
    /// own preselection.
    /// </summary>
    internal string? PendingPreselectDeviceOverride { get; set; }

    internal bool PendingPreselectIsLoopback { get; set; }

    private bool _pendingPreselectConsumed;

    /// <summary>
    /// Resolved capture device name to forward to the decoder (empty = host
    /// default). Computed from <see cref="SelectedRadioMonitorDevice"/>.
    /// </summary>
    public string ResolvedCaptureDevice =>
        SelectedRadioMonitorDevice is null
        || ReferenceEquals(SelectedRadioMonitorDevice, RadioMonitorDeviceCatalog.SystemDefault)
        || string.Equals(SelectedRadioMonitorDevice.Name, RadioMonitorDeviceCatalog.SystemDefault.Name, StringComparison.Ordinal)
            ? string.Empty
            : SelectedRadioMonitorDevice.Name;

    /// <summary>
    /// Whether the resolved capture device is a system OUTPUT being captured
    /// via WASAPI loopback. Computed from the dropdown selection.
    /// </summary>
    public bool ResolvedIsLoopback => SelectedRadioMonitorDevice?.IsLoopback ?? false;

    [ObservableProperty]
    private int _selectedSectionIndex;

    // UI state
    [ObservableProperty]
    private bool _isLoading;

    [ObservableProperty]
    private bool _isSaving;

    [ObservableProperty]
    private string? _errorMessage;

    /// <summary>
    /// True after a successful save. Checked by the caller after the dialog closes.
    /// </summary>
    [ObservableProperty]
    private bool _didSave;

    private bool _hasPersistedRigControl;

    public ObservableCollection<PersistenceSetupField> PersistenceFields { get; } = [];

    public bool HasPersistenceInputs => PersistenceFields.Count > 0;

    public bool ShowsPersistenceInfoOnly => !HasPersistenceInputs;

    public bool RequiresLogFilePath => PersistenceFields.Count == 1 && PersistenceFields[0].IsPath;

    public string LogFilePath
    {
        get => PersistenceSetupFields.GetPathValue(PersistenceFields) ?? string.Empty;
        set
        {
            if (PersistenceFields.FirstOrDefault(persistenceField => persistenceField.IsPath) is { } pathField)
            {
                pathField.Value = value;
                OnPropertyChanged();
            }
        }
    }

    /// <summary>
    /// Raised when the dialog should close. The bool parameter is true for save, false for cancel.
    /// </summary>
    internal event EventHandler<bool>? CloseRequested;

    public SettingsViewModel(IEngineClient engine)
    {
        _engine = engine;
        CatHubFaces.CollectionChanged += OnCatHubCollectionChanged;
        CatHubEndpoints.CollectionChanged += OnCatHubCollectionChanged;
    }

    private static readonly HashSet<string> CatHubScalarPropertyNames = new(StringComparer.Ordinal)
    {
        nameof(CatHubBackend),
        nameof(CatHubModel),
        nameof(CatHubTransport),
        nameof(CatHubPort),
        nameof(CatHubBaud),
        nameof(CatHubHost),
        nameof(CatHubTcpPort),
        nameof(CatHubCertifiedIndex),
        nameof(CatHubReplyTimeoutMs),
        nameof(CatHubPollBaselineMs),
        nameof(CatHubPollHeartbeatMs),
        nameof(CatHubPttMaxTxMs),
        nameof(CatHubNativePushIndex),
    };

    private static readonly HashSet<string> WsjtxIngestScalarPropertyNames = new(StringComparer.Ordinal)
    {
        nameof(WsjtxIngestEnabled),
        nameof(WsjtxUdpEnabled),
        nameof(WsjtxUdpBind),
        nameof(WsjtxAdifTailEnabled),
        nameof(WsjtxAdifTailPath),
        nameof(WsjtxPollIntervalMs),
        nameof(WsjtxSyncToQrz),
    };

    protected override void OnPropertyChanged(PropertyChangedEventArgs e)
    {
        base.OnPropertyChanged(e);
        if (e.PropertyName is not null && CatHubScalarPropertyNames.Contains(e.PropertyName))
        {
            MarkCatHubDirty();
        }

        if (e.PropertyName is not null && WsjtxIngestScalarPropertyNames.Contains(e.PropertyName))
        {
            MarkWsjtxIngestDirty();
        }
    }

    private void MarkCatHubDirty()
    {
        if (_catHubLoading)
        {
            return;
        }

        IsCatHubDirty = true;
    }

    private void MarkWsjtxIngestDirty()
    {
        if (_wsjtxIngestLoading)
        {
            return;
        }

        IsWsjtxIngestDirty = true;
    }

    private void OnCatHubCollectionChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        if (e.OldItems is not null)
        {
            foreach (ObservableObject row in e.OldItems)
            {
                row.PropertyChanged -= OnCatHubRowChanged;
            }
        }

        if (e.NewItems is not null)
        {
            foreach (ObservableObject row in e.NewItems)
            {
                row.PropertyChanged += OnCatHubRowChanged;
            }
        }

        MarkCatHubDirty();
    }

    private void OnCatHubRowChanged(object? sender, PropertyChangedEventArgs e) => MarkCatHubDirty();

    [RelayCommand]
    private void AddCatHubFace()
    {
        CatHubFaces.Add(new CatHubFaceRowViewModel());
    }

    [RelayCommand]
    private void RemoveCatHubFace(CatHubFaceRowViewModel? row)
    {
        if (row is not null)
        {
            CatHubFaces.Remove(row);
        }
    }

    [RelayCommand]
    private void AddCatHubEndpoint()
    {
        CatHubEndpoints.Add(new CatHubEndpointRowViewModel());
    }

    [RelayCommand]
    private void RemoveCatHubEndpoint(CatHubEndpointRowViewModel? row)
    {
        if (row is not null)
        {
            CatHubEndpoints.Remove(row);
        }
    }

    /// <summary>
    /// Loads current settings from the engine. Call after construction.
    /// </summary>
    internal async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            var state = await _engine.GetWizardStateAsync();
            ApplyStatus(state.Status);

            var activeProfile = state.StationProfiles
                .FirstOrDefault(p => p.IsActive)?.Profile
                ?? state.Status.StationProfile;

            if (activeProfile is not null)
            {
                ApplyStationProfile(activeProfile);
            }
        }
        catch (Grpc.Core.RpcException ex)
        {
            ErrorMessage = $"Failed to load settings: {ex.Status.Detail}";
        }
        finally
        {
            IsLoading = false;
        }

        await RefreshRadioMonitorDevicesAsync().ConfigureAwait(true);
    }

    /// <summary>
    /// Sets the dropdown's pre-selection from the current persisted device
    /// override + loopback flag. Call after constructing the view-model.
    /// Matching is by (name, isLoopback) so users see the same entry they
    /// previously picked, even if multiple devices share a substring.
    /// </summary>
    internal void PreselectRadioMonitorDevice(string? deviceOverride, bool isLoopback)
    {
        if (string.IsNullOrWhiteSpace(deviceOverride))
        {
            SelectedRadioMonitorDevice = RadioMonitorDeviceCatalog.SystemDefault;
            return;
        }

        var match = RadioMonitorDevices.FirstOrDefault(d =>
            !ReferenceEquals(d, RadioMonitorDeviceCatalog.SystemDefault)
            && string.Equals(d.Name, deviceOverride, StringComparison.Ordinal)
            && d.IsLoopback == isLoopback);

        if (match is null)
        {
            // Persisted device is no longer present in the enumeration (radio
            // unplugged, etc.). Insert a synthetic entry so the user keeps the
            // visible state but can still pick a different one. The synthetic
            // entry's Name stays clean (so cw-decoder's substring matching can
            // still succeed if the device reappears), and the
            // "(not currently available)" suffix lives only in DisplayName.
            match = new RadioMonitorDevice(deviceOverride, isLoopback, IsUnavailable: true);
            RadioMonitorDevices.Add(match);
        }

        SelectedRadioMonitorDevice = match;
    }

    [RelayCommand]
    internal async Task RefreshRadioMonitorDevicesAsync()
    {
        if (IsLoadingRadioMonitorDevices)
        {
            return;
        }

        IsLoadingRadioMonitorDevices = true;
        try
        {
            var binary = CwDecoderProcessSampleSource.LocateBinary();
            RadioMonitorBinaryStatus = binary is null
                ? "Decoder not built — see experiments/cw-decoder/README.md"
                : $"Decoder: {binary}";

            var previousSelection = SelectedRadioMonitorDevice;
            var devices = await RadioMonitorDeviceCatalog.ListAsync().ConfigureAwait(true);

            RadioMonitorDevices.Clear();
            foreach (var device in devices)
            {
                RadioMonitorDevices.Add(device);
            }

            // First refresh after construction: honor the caller's persisted
            // preselection (set by MainWindowViewModel.CreateSettingsViewModel).
            // This must run BEFORE the "preserve previousSelection" logic and
            // BEFORE the SystemDefault fallback so we don't auto-select an
            // unrelated entry.
            if (!_pendingPreselectConsumed)
            {
                _pendingPreselectConsumed = true;
                PreselectRadioMonitorDevice(PendingPreselectDeviceOverride, PendingPreselectIsLoopback);
                return;
            }

            // Preserve user's selection across explicit refresh.
            if (previousSelection is not null)
            {
                var match = RadioMonitorDevices.FirstOrDefault(d =>
                    string.Equals(d.Name, previousSelection.Name, StringComparison.Ordinal)
                    && d.IsLoopback == previousSelection.IsLoopback);
                SelectedRadioMonitorDevice = match ?? RadioMonitorDeviceCatalog.SystemDefault;
            }
            else if (RadioMonitorDevices.Count > 0)
            {
                SelectedRadioMonitorDevice = RadioMonitorDevices[0];
            }
        }
        finally
        {
            IsLoadingRadioMonitorDevices = false;
        }
    }

    [RelayCommand]
    private async Task SaveAsync()
    {
        IsSaving = true;
        ErrorMessage = null;
        try
        {
            if (!TryValidateRigControlInputs(out var validationError))
            {
                ErrorMessage = validationError;
                return;
            }

            if (IsCatHubDirty && !TryValidateCatHubInputs(out var catHubError))
            {
                ErrorMessage = catHubError;
                return;
            }

            if (IsWsjtxIngestDirty && !TryValidateWsjtxIngestInputs(out var wsjtxError))
            {
                ErrorMessage = wsjtxError;
                return;
            }

            var request = BuildSaveRequest();
            await _engine.SaveSetupAsync(request);
            DidSave = true;
            CloseRequested?.Invoke(this, true);
        }
        catch (Grpc.Core.RpcException ex)
        {
            ErrorMessage = $"Save failed: {ex.Status.Detail}";
        }
        finally
        {
            IsSaving = false;
        }
    }

    [RelayCommand]
    private void Cancel()
    {
        CloseRequested?.Invoke(this, false);
    }

    [RelayCommand]
    private void SelectStationSection() => SelectedSectionIndex = 0;

    [RelayCommand]
    private void SelectDisplaySection() => SelectedSectionIndex = 1;

    [RelayCommand]
    private void SelectStorageSyncSection() => SelectedSectionIndex = 2;

    [RelayCommand]
    private void SelectQrzSection() => SelectedSectionIndex = 3;

    [RelayCommand]
    private void SelectRigSection() => SelectedSectionIndex = 4;

    [RelayCommand]
    private void SelectCatHubSection() => SelectedSectionIndex = 5;

    [RelayCommand]
    private void SelectWsjtxSection() => SelectedSectionIndex = 6;

    [RelayCommand]
    private void SelectNextSection() => SelectedSectionIndex = (SelectedSectionIndex + 1) % 7;

    [RelayCommand]
    private void SelectPreviousSection() => SelectedSectionIndex = (SelectedSectionIndex + 6) % 7;

    [RelayCommand]
    private async Task TestQrzXmlAsync()
    {
        if (string.IsNullOrWhiteSpace(QrzXmlUsername) || string.IsNullOrWhiteSpace(QrzXmlPassword))
        {
            QrzXmlTestResult = "Username and password are required.";
            QrzXmlTestSucceeded = false;
            return;
        }

        IsTestingQrzXml = true;
        QrzXmlTestResult = null;
        try
        {
            var result = await _engine.TestQrzCredentialsAsync(QrzXmlUsername, QrzXmlPassword);
            QrzXmlTestSucceeded = result.Success;
            QrzXmlTestResult = result.Success
                ? "✓ Connected to QRZ XML successfully!"
                : $"✗ {result.ErrorMessage}";
        }
        catch (Grpc.Core.RpcException ex)
        {
            QrzXmlTestSucceeded = false;
            QrzXmlTestResult = $"✗ Connection failed: {ex.Status.Detail}";
        }
        finally
        {
            IsTestingQrzXml = false;
        }
    }

    [RelayCommand]
    private async Task TestQrzLogbookAsync()
    {
        if (string.IsNullOrWhiteSpace(QrzLogbookApiKey))
        {
            LogbookTestResult = "API key is required.";
            LogbookTestSucceeded = false;
            return;
        }

        IsTestingLogbook = true;
        LogbookTestResult = null;
        try
        {
            var result = await _engine.TestQrzLogbookCredentialsAsync(QrzLogbookApiKey);
            LogbookTestSucceeded = result.Success;
            if (result.Success)
            {
                var owner = string.IsNullOrWhiteSpace(result.LogbookOwner)
                    ? string.Empty
                    : $" ({result.LogbookOwner})";
                var count = result.HasQsoCount
                    ? $" — {result.QsoCount} QSOs"
                    : string.Empty;
                LogbookTestResult = $"✓ Logbook connected{owner}{count}";
            }
            else
            {
                LogbookTestResult = $"✗ {result.ErrorMessage}";
            }
        }
        catch (Grpc.Core.RpcException ex)
        {
            LogbookTestSucceeded = false;
            LogbookTestResult = $"✗ Connection failed: {ex.Status.Detail}";
        }
        finally
        {
            IsTestingLogbook = false;
        }
    }

    private void ApplyStatus(SetupStatus status)
    {
        QrzXmlUsername = status.QrzXmlUsername ?? string.Empty;
        PersistenceSectionTitle = string.IsNullOrWhiteSpace(status.PersistenceLabel)
            ? "Storage"
            : status.PersistenceLabel;
        PersistenceDescription = string.IsNullOrWhiteSpace(status.PersistenceDescription)
            ? "Where should QsoRipper store persisted logbook data?"
            : status.PersistenceDescription;
        ReplacePersistenceFields(PersistenceSetupFields.FromStatus(status, status.SuggestedLogFilePath ?? string.Empty));
        _hasPersistedRigControl = status.RigControl is not null;

        if (status.SyncConfig is not null)
        {
            AutoSyncEnabled = status.SyncConfig.AutoSyncEnabled;
            SyncIntervalSeconds = status.SyncConfig.SyncIntervalSeconds > 0
                ? (int)status.SyncConfig.SyncIntervalSeconds
                : 300;
            ConflictPolicy = status.SyncConfig.ConflictPolicy;
        }

        if (status.RigControl is not null)
        {
            RigControlEnabled = status.RigControl.Enabled;
            RigControlHost = status.RigControl.HasHost
                ? status.RigControl.Host
                : string.Empty;
            RigControlPort = status.RigControl.HasPort
                ? status.RigControl.Port.ToString(CultureInfo.InvariantCulture)
                : string.Empty;
            RigControlReadTimeoutMs = status.RigControl.HasReadTimeoutMs
                ? status.RigControl.ReadTimeoutMs.ToString(CultureInfo.InvariantCulture)
                : string.Empty;
            RigControlStaleThresholdMs = status.RigControl.HasStaleThresholdMs
                ? status.RigControl.StaleThresholdMs.ToString(CultureInfo.InvariantCulture)
                : string.Empty;
        }
        else
        {
            RigControlEnabled = false;
            RigControlHost = string.Empty;
            RigControlPort = string.Empty;
            RigControlReadTimeoutMs = string.Empty;
            RigControlStaleThresholdMs = string.Empty;
        }

        // Password and API key are never returned by the engine for security;
        // leave them empty so the user can re-enter if they want to change them.
        ApplyCatHub(status.CatHub);
        ApplyWsjtxIngest(status.WsjtxIngest);
    }

    private void ApplyWsjtxIngest(WsjtxIngestSettings? settings)
    {
        _wsjtxIngestLoading = true;
        try
        {
            _hasPersistedWsjtxIngest = settings is not null;
            WsjtxIngestEnabled = settings?.Enabled ?? false;
            WsjtxUdpEnabled = settings is { HasUdpEnabled: true } ? settings.UdpEnabled : true;
            WsjtxUdpBind = !string.IsNullOrWhiteSpace(settings?.UdpBind)
                ? settings.UdpBind
                : WsjtxIngestSetup.DefaultUdpBind;
            WsjtxAdifTailEnabled = settings?.AdifTailEnabled ?? false;
            WsjtxAdifTailPath = settings is { HasAdifTailPath: true }
                ? settings.AdifTailPath
                : string.Empty;
            WsjtxPollIntervalMs = settings is { PollIntervalMs: > 0 }
                ? settings.PollIntervalMs.ToString(CultureInfo.InvariantCulture)
                : WsjtxIngestSetup.DefaultPollIntervalMs.ToString(CultureInfo.InvariantCulture);
            WsjtxSyncToQrz = settings?.SyncToQrz ?? false;
        }
        finally
        {
            _wsjtxIngestLoading = false;
            IsWsjtxIngestDirty = false;
        }
    }

    private void ApplyCatHub(CatHubSettings? catHub)
    {
        _catHubLoading = true;
        try
        {
            foreach (var row in CatHubFaces)
            {
                row.PropertyChanged -= OnCatHubRowChanged;
            }

            foreach (var row in CatHubEndpoints)
            {
                row.PropertyChanged -= OnCatHubRowChanged;
            }

            CatHubFaces.Clear();
            CatHubEndpoints.Clear();

            _hasPersistedCatHub = catHub is not null;

            var radio = catHub?.Radio;
            CatHubBackend = radio is { HasBackend: true } ? radio.Backend : string.Empty;
            CatHubModel = radio is { HasModel: true } ? radio.Model : string.Empty;
            CatHubTransport = radio is { HasTransport: true } ? radio.Transport : string.Empty;
            CatHubPort = radio is { HasPort: true } ? radio.Port : string.Empty;
            CatHubBaud = radio is { HasBaud: true } ? radio.Baud.ToString(CultureInfo.InvariantCulture) : string.Empty;
            CatHubHost = radio is { HasHost: true } ? radio.Host : string.Empty;
            CatHubTcpPort = radio is { HasTcpPort: true } ? radio.TcpPort.ToString(CultureInfo.InvariantCulture) : string.Empty;
            CatHubCertifiedIndex = radio is { HasCertified: true } ? (radio.Certified ? 1 : 2) : 0;
            CatHubReplyTimeoutMs = radio is { HasReplyTimeoutMs: true }
                ? radio.ReplyTimeoutMs.ToString(CultureInfo.InvariantCulture) : string.Empty;

            var poll = catHub?.Poll;
            CatHubPollBaselineMs = poll is { HasBaselineMs: true }
                ? poll.BaselineMs.ToString(CultureInfo.InvariantCulture) : string.Empty;
            CatHubPollHeartbeatMs = poll is { HasHeartbeatMs: true }
                ? poll.HeartbeatMs.ToString(CultureInfo.InvariantCulture) : string.Empty;

            var ptt = catHub?.Ptt;
            CatHubPttMaxTxMs = ptt is { HasMaxTxMs: true }
                ? ptt.MaxTxMs.ToString(CultureInfo.InvariantCulture) : string.Empty;

            var events = catHub?.Events;
            CatHubNativePushIndex = events is { HasNativePush: true } ? (events.NativePush ? 1 : 2) : 0;

            if (catHub is not null)
            {
                foreach (var face in catHub.Faces)
                {
                    CatHubFaces.Add(new CatHubFaceRowViewModel
                    {
                        Name = face.Name,
                        Transport = face.Transport,
                        Baud = face.Baud != 0 ? face.Baud.ToString(CultureInfo.InvariantCulture) : string.Empty,
                        Dialect = string.IsNullOrWhiteSpace(face.Dialect) ? "ts590" : face.Dialect,
                        PermRead = face.Perms.Contains(CatHubPermission.Read),
                        PermWrite = face.Perms.Contains(CatHubPermission.Write),
                        PermPtt = face.Perms.Contains(CatHubPermission.Ptt),
                        PermConfigWrite = face.Perms.Contains(CatHubPermission.ConfigWrite),
                    });
                }

                foreach (var endpoint in catHub.HamlibNet)
                {
                    CatHubEndpoints.Add(new CatHubEndpointRowViewModel
                    {
                        Name = endpoint.Name,
                        Bind = endpoint.Bind,
                        PermRead = endpoint.Perms.Contains(CatHubPermission.Read),
                        PermWrite = endpoint.Perms.Contains(CatHubPermission.Write),
                        PermPtt = endpoint.Perms.Contains(CatHubPermission.Ptt),
                        PermConfigWrite = endpoint.Perms.Contains(CatHubPermission.ConfigWrite),
                    });
                }
            }
        }
        finally
        {
            _catHubLoading = false;
            IsCatHubDirty = false;
        }
    }

    private void ApplyStationProfile(StationProfile profile)
    {
        ProfileName = profile.ProfileName ?? string.Empty;
        Callsign = profile.StationCallsign ?? string.Empty;
        OperatorCallsign = profile.OperatorCallsign ?? string.Empty;
        OperatorName = profile.OperatorName ?? string.Empty;
        GridSquare = profile.Grid ?? string.Empty;
        County = profile.County ?? string.Empty;
        State = profile.State ?? string.Empty;
        Country = profile.Country ?? string.Empty;
        ArrlSection = profile.ArrlSection ?? string.Empty;
        Dxcc = profile.Dxcc != 0
            ? profile.Dxcc.ToString(CultureInfo.InvariantCulture) : string.Empty;
        CqZone = profile.CqZone != 0
            ? profile.CqZone.ToString(CultureInfo.InvariantCulture) : string.Empty;
        ItuZone = profile.ItuZone != 0
            ? profile.ItuZone.ToString(CultureInfo.InvariantCulture) : string.Empty;
        Latitude = profile.Latitude != 0
            ? profile.Latitude.ToString(CultureInfo.InvariantCulture) : string.Empty;
        Longitude = profile.Longitude != 0
            ? profile.Longitude.ToString(CultureInfo.InvariantCulture) : string.Empty;
    }

    private SaveSetupRequest BuildSaveRequest()
    {
        var profile = new StationProfile
        {
            StationCallsign = Callsign.Trim(),
            Grid = GridSquare.Trim(),
            OperatorName = OperatorName.Trim(),
        };

        SetOptionalString(ProfileName, v => profile.ProfileName = v);
        SetOptionalString(OperatorCallsign, v => profile.OperatorCallsign = v);
        SetOptionalString(County, v => profile.County = v);
        SetOptionalString(State, v => profile.State = v);
        SetOptionalString(Country, v => profile.Country = v);
        SetOptionalString(ArrlSection, v => profile.ArrlSection = v);
        SetUintField(Dxcc, v => profile.Dxcc = v);
        SetUintField(CqZone, v => profile.CqZone = v);
        SetUintField(ItuZone, v => profile.ItuZone = v);
        SetDoubleField(Latitude, v => profile.Latitude = v);
        SetDoubleField(Longitude, v => profile.Longitude = v);

        var request = new SaveSetupRequest
        {
            StationProfile = profile,
            SyncConfig = new SyncConfig
            {
                AutoSyncEnabled = AutoSyncEnabled,
                SyncIntervalSeconds = SyncIntervalSeconds > 0
                    ? (uint)SyncIntervalSeconds : 300,
                ConflictPolicy = ConflictPolicy,
            },
        };

        PersistenceSetupFields.ApplyTo(request, PersistenceFields);

        if (!string.IsNullOrWhiteSpace(QrzXmlUsername))
        {
            request.QrzXmlUsername = QrzXmlUsername.Trim();
            if (!string.IsNullOrEmpty(QrzXmlPassword))
            {
                request.QrzXmlPassword = QrzXmlPassword;
            }
        }

        if (!string.IsNullOrWhiteSpace(QrzLogbookApiKey))
        {
            request.QrzLogbookApiKey = QrzLogbookApiKey.Trim();
        }

        var rigControl = BuildRigControlSettings();
        if (rigControl is not null)
        {
            request.RigControl = rigControl;
        }

        if (IsCatHubDirty)
        {
            request.CatHub = BuildCatHubSettings();
        }

        if (IsWsjtxIngestDirty)
        {
            request.WsjtxIngest = BuildWsjtxIngestSettings();
        }

        return request;
    }

    private WsjtxIngestSettings BuildWsjtxIngestSettings()
    {
        var settings = new WsjtxIngestSettings
        {
            Enabled = WsjtxIngestEnabled,
            UdpEnabled = WsjtxUdpEnabled,
            AdifTailEnabled = WsjtxAdifTailEnabled,
            SyncToQrz = WsjtxSyncToQrz,
        };

        SetOptionalString(WsjtxUdpBind, value => settings.UdpBind = value);
        SetOptionalString(WsjtxAdifTailPath, value => settings.AdifTailPath = value);
        SetUInt32Field(WsjtxPollIntervalMs, value => settings.PollIntervalMs = value);
        return settings;
    }

    private CatHubSettings BuildCatHubSettings()
    {
        var settings = new CatHubSettings();

        var radio = new CatHubRadioSettings();
        SetOptionalString(CatHubBackend, value => radio.Backend = value.ToLowerInvariant());
        SetOptionalString(CatHubModel, value => radio.Model = value);
        SetOptionalString(CatHubTransport, value => radio.Transport = value.ToLowerInvariant());
        SetOptionalString(CatHubPort, value => radio.Port = value);
        SetUInt32Field(CatHubBaud, value => radio.Baud = value);
        SetOptionalString(CatHubHost, value => radio.Host = value);
        SetUInt32Field(CatHubTcpPort, value => radio.TcpPort = value);
        if (CatHubCertifiedIndex != 0)
        {
            radio.Certified = CatHubCertifiedIndex == 1;
        }

        SetUInt64Field(CatHubReplyTimeoutMs, value => radio.ReplyTimeoutMs = value);
        settings.Radio = radio;

        if (!string.IsNullOrWhiteSpace(CatHubPollBaselineMs) || !string.IsNullOrWhiteSpace(CatHubPollHeartbeatMs))
        {
            var poll = new CatHubPollSettings();
            SetUInt64Field(CatHubPollBaselineMs, value => poll.BaselineMs = value);
            SetUInt64Field(CatHubPollHeartbeatMs, value => poll.HeartbeatMs = value);
            settings.Poll = poll;
        }

        if (!string.IsNullOrWhiteSpace(CatHubPttMaxTxMs))
        {
            var ptt = new CatHubPttSettings();
            SetUInt64Field(CatHubPttMaxTxMs, value => ptt.MaxTxMs = value);
            settings.Ptt = ptt;
        }

        if (CatHubNativePushIndex != 0)
        {
            settings.Events = new CatHubEventSettings { NativePush = CatHubNativePushIndex == 1 };
        }

        foreach (var face in CatHubFaces)
        {
            var proto = new CatHubSerialFace
            {
                Name = face.Name.Trim(),
                Transport = face.Transport.Trim(),
                Dialect = string.IsNullOrWhiteSpace(face.Dialect) ? "ts590" : face.Dialect.Trim().ToLowerInvariant(),
            };
            if (uint.TryParse(face.Baud, CultureInfo.InvariantCulture, out var baud))
            {
                proto.Baud = baud;
            }

            AddPerms(proto.Perms, face.PermRead, face.PermWrite, face.PermPtt, face.PermConfigWrite);
            settings.Faces.Add(proto);
        }

        foreach (var endpoint in CatHubEndpoints)
        {
            var proto = new CatHubHamlibNetEndpoint
            {
                Name = endpoint.Name.Trim(),
                Bind = endpoint.Bind.Trim(),
            };
            AddPerms(proto.Perms, endpoint.PermRead, endpoint.PermWrite, endpoint.PermPtt, endpoint.PermConfigWrite);
            settings.HamlibNet.Add(proto);
        }

        return settings;
    }

    private static void AddPerms(
        Google.Protobuf.Collections.RepeatedField<CatHubPermission> perms,
        bool read,
        bool write,
        bool ptt,
        bool configWrite)
    {
        if (read)
        {
            perms.Add(CatHubPermission.Read);
        }

        if (write)
        {
            perms.Add(CatHubPermission.Write);
        }

        if (ptt)
        {
            perms.Add(CatHubPermission.Ptt);
        }

        if (configWrite)
        {
            perms.Add(CatHubPermission.ConfigWrite);
        }
    }

    private RigControlSettings? BuildRigControlSettings()
    {
        var hasExplicitValues = RigControlEnabled
            || !string.IsNullOrWhiteSpace(RigControlHost)
            || !string.IsNullOrWhiteSpace(RigControlPort)
            || !string.IsNullOrWhiteSpace(RigControlReadTimeoutMs)
            || !string.IsNullOrWhiteSpace(RigControlStaleThresholdMs);

        if (!_hasPersistedRigControl && !hasExplicitValues)
        {
            return null;
        }

        var settings = new RigControlSettings();
        if (_hasPersistedRigControl || RigControlEnabled)
        {
            settings.Enabled = RigControlEnabled;
        }

        SetOptionalString(RigControlHost, value => settings.Host = value);
        SetUInt32Field(RigControlPort, value => settings.Port = value);
        SetUInt64Field(RigControlReadTimeoutMs, value => settings.ReadTimeoutMs = value);
        SetUInt64Field(RigControlStaleThresholdMs, value => settings.StaleThresholdMs = value);
        return settings;
    }

    private void ReplacePersistenceFields(IReadOnlyList<PersistenceSetupField> fields)
    {
        PersistenceFields.Clear();
        foreach (var field in fields)
        {
            PersistenceFields.Add(field);
        }

        OnPropertyChanged(nameof(PersistenceFields));
        OnPropertyChanged(nameof(HasPersistenceInputs));
        OnPropertyChanged(nameof(ShowsPersistenceInfoOnly));
        OnPropertyChanged(nameof(RequiresLogFilePath));
        OnPropertyChanged(nameof(LogFilePath));
    }

    private bool TryValidateRigControlInputs(out string? validationError)
    {
        if (!TryValidateUInt32Field(
                RigControlPort,
                1,
                65_535,
                "Rig control port",
                out validationError))
        {
            return false;
        }

        if (!TryValidateUInt64Field(
                RigControlReadTimeoutMs,
                1,
                "Rig control read timeout",
                out validationError))
        {
            return false;
        }

        return TryValidateUInt64Field(
            RigControlStaleThresholdMs,
            1,
            "Rig control stale threshold",
            out validationError);
    }

    private bool TryValidateWsjtxIngestInputs(out string? validationError)
    {
        validationError = null;

        if (!string.IsNullOrWhiteSpace(WsjtxUdpBind)
            && !WsjtxIngestSetup.TryValidateHostPort(
                WsjtxUdpBind,
                "WSJT-X UDP bind",
                out validationError))
        {
            return false;
        }

        if (WsjtxAdifTailEnabled && string.IsNullOrWhiteSpace(WsjtxAdifTailPath))
        {
            validationError = "WSJT-X ADIF tail path is required when ADIF tail recovery is enabled.";
            return false;
        }

        if (!uint.TryParse(WsjtxPollIntervalMs, CultureInfo.InvariantCulture, out var pollInterval)
            && !string.IsNullOrWhiteSpace(WsjtxPollIntervalMs))
        {
            validationError = "WSJT-X poll interval must be a whole number.";
            return false;
        }

        if (!string.IsNullOrWhiteSpace(WsjtxPollIntervalMs)
            && !WsjtxIngestSetup.IsValidPollInterval(pollInterval))
        {
            validationError = "WSJT-X poll interval must be 0 for the engine default or a positive whole number.";
            return false;
        }

        return true;
    }

    private bool TryValidateCatHubInputs(out string? validationError)
    {
        validationError = null;

        var backend = CatHubBackend.Trim().ToLowerInvariant();
        var managed = backend is not ("" or "loopback");
        if (managed && backend is not ("ts590" or "rigctld"))
        {
            validationError = "CAT hub backend must be ts590, rigctld, or loopback.";
            return false;
        }

        var transport = CatHubTransport.Trim().ToLowerInvariant();
        if (transport.Length > 0 && transport is not ("serial" or "tcp"))
        {
            validationError = "CAT hub transport must be serial or tcp.";
            return false;
        }

        if (managed && transport == "serial" && string.IsNullOrWhiteSpace(CatHubPort))
        {
            validationError = "CAT hub serial transport requires a port (e.g. COM3).";
            return false;
        }

        if (!TryValidateUInt32Field(CatHubBaud, 1, 4_000_000, "CAT hub baud", out validationError))
        {
            return false;
        }

        if (!TryValidateUInt32Field(CatHubTcpPort, 1, 65_535, "CAT hub TCP port", out validationError))
        {
            return false;
        }

        if (!TryValidateUInt64Field(CatHubReplyTimeoutMs, 1, "CAT hub reply timeout", out validationError)
            || !TryValidateUInt64Field(CatHubPollBaselineMs, 1, "CAT hub poll baseline", out validationError)
            || !TryValidateUInt64Field(CatHubPollHeartbeatMs, 1, "CAT hub poll heartbeat", out validationError)
            || !TryValidateUInt64Field(CatHubPttMaxTxMs, 1, "CAT hub PTT max TX", out validationError))
        {
            return false;
        }

        var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var face in CatHubFaces)
        {
            if (string.IsNullOrWhiteSpace(face.Name))
            {
                validationError = "Every CAT hub face needs a name.";
                return false;
            }

            if (string.IsNullOrWhiteSpace(face.Transport))
            {
                validationError = $"CAT hub face '{face.Name}' needs a transport (path or host:port).";
                return false;
            }

            var dialect = face.Dialect.Trim().ToLowerInvariant();
            if (dialect is not ("ts590" or "ts2000"))
            {
                validationError = $"CAT hub face '{face.Name}' dialect must be ts590 or ts2000.";
                return false;
            }

            if (!names.Add(face.Name.Trim()))
            {
                validationError = $"CAT hub endpoint name '{face.Name}' is used more than once.";
                return false;
            }
        }

        foreach (var endpoint in CatHubEndpoints)
        {
            if (string.IsNullOrWhiteSpace(endpoint.Name))
            {
                validationError = "Every CAT hub network endpoint needs a name.";
                return false;
            }

            if (string.IsNullOrWhiteSpace(endpoint.Bind) || !endpoint.Bind.Contains(':', StringComparison.Ordinal))
            {
                validationError = $"CAT hub endpoint '{endpoint.Name}' bind must be host:port (e.g. 127.0.0.1:4532).";
                return false;
            }

            if (!names.Add(endpoint.Name.Trim()))
            {
                validationError = $"CAT hub endpoint name '{endpoint.Name}' is used more than once.";
                return false;
            }
        }

        if (managed && CatHubEndpoints.Count == 0 && CatHubFaces.Count == 0)
        {
            validationError = "A managed CAT hub radio needs at least one face or network endpoint.";
            return false;
        }

        return true;
    }

    private static void SetOptionalString(string input, Action<string> setter)
    {
        if (!string.IsNullOrWhiteSpace(input))
        {
            setter(input.Trim());
        }
    }

    private static void SetUintField(string? input, Action<uint> setter)
    {
        if (uint.TryParse(input, CultureInfo.InvariantCulture, out var value))
        {
            setter(value);
        }
    }

    private static void SetUInt32Field(string? input, Action<uint> setter)
    {
        if (uint.TryParse(input, CultureInfo.InvariantCulture, out var value))
        {
            setter(value);
        }
    }

    private static void SetUInt64Field(string? input, Action<ulong> setter)
    {
        if (ulong.TryParse(input, CultureInfo.InvariantCulture, out var value))
        {
            setter(value);
        }
    }

    private static bool TryValidateUInt32Field(
        string? input,
        uint min,
        uint max,
        string label,
        out string? errorMessage)
    {
        errorMessage = null;
        if (string.IsNullOrWhiteSpace(input))
        {
            return true;
        }

        if (!uint.TryParse(input, CultureInfo.InvariantCulture, out var value)
            || value < min
            || value > max)
        {
            errorMessage = $"{label} must be a whole number between {min} and {max}.";
            return false;
        }

        return true;
    }

    private static bool TryValidateUInt64Field(
        string? input,
        ulong min,
        string label,
        out string? errorMessage)
    {
        errorMessage = null;
        if (string.IsNullOrWhiteSpace(input))
        {
            return true;
        }

        if (!ulong.TryParse(input, CultureInfo.InvariantCulture, out var value) || value < min)
        {
            errorMessage = $"{label} must be a whole number greater than or equal to {min}.";
            return false;
        }

        return true;
    }

    private static void SetDoubleField(string? input, Action<double> setter)
    {
        if (double.TryParse(input, CultureInfo.InvariantCulture, out var value))
        {
            setter(value);
        }
    }

}
