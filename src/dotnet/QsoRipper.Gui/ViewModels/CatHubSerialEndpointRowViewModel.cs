using CommunityToolkit.Mvvm.ComponentModel;

namespace QsoRipper.Gui.ViewModels;

/// <summary>
/// One editable serial-endpoint row (<c>[[cat_hub.serial_endpoint]]</c>) in the CAT Hub
/// settings tab. A serial endpoint is a com0com virtual pair the hub binds; the
/// client application connects to the other port of the same pair.
/// </summary>
internal sealed partial class CatHubSerialEndpointRowViewModel : ObservableObject
{
    [ObservableProperty]
    private string _name = string.Empty;

    [ObservableProperty]
    private string _transport = string.Empty;

    [ObservableProperty]
    private string _applicationTransport = string.Empty;

    [ObservableProperty]
    private string _baud = string.Empty;

    [ObservableProperty]
    private string _dialect = "ts590";

    public string[] DialectOptions { get; } = ["ts590", "ts2000"];

    [ObservableProperty]
    private bool _permRead = true;

    [ObservableProperty]
    private bool _permWrite;

    [ObservableProperty]
    private bool _permPtt;

    [ObservableProperty]
    private bool _permConfigWrite;
}
