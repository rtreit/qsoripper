using CommunityToolkit.Mvvm.ComponentModel;

namespace QsoRipper.Gui.ViewModels;

/// <summary>
/// One editable rigctld-compatible TCP endpoint row
/// (<c>[[cat_hub.hamlib_net]]</c>) in the CAT Hub settings tab. Network-CAT
/// applications (the engine, WSJT-X, Log4OM, ...) connect to the bind address.
/// </summary>
internal sealed partial class CatHubEndpointRowViewModel : ObservableObject
{
    [ObservableProperty]
    private string _name = string.Empty;

    [ObservableProperty]
    private string _bind = string.Empty;

    [ObservableProperty]
    private bool _permRead = true;

    [ObservableProperty]
    private bool _permWrite;

    [ObservableProperty]
    private bool _permPtt;

    [ObservableProperty]
    private bool _permConfigWrite;
}
