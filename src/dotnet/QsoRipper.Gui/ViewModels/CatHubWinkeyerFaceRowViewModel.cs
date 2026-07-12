using CommunityToolkit.Mvvm.ComponentModel;

namespace QsoRipper.Gui.ViewModels;

/// <summary>One editable <c>[[cat_hub.winkeyer_face]]</c> virtual serial endpoint.</summary>
internal sealed partial class CatHubWinkeyerFaceRowViewModel : ObservableObject
{
    [ObservableProperty] private string _name = string.Empty;
    [ObservableProperty] private string _transport = string.Empty;
    [ObservableProperty] private string _applicationTransport = string.Empty;
    [ObservableProperty] private string _baud = "1200";
    [ObservableProperty] private bool _primary;
    [ObservableProperty] private bool _permStatus = true;
    [ObservableProperty] private bool _permSend = true;
    [ObservableProperty] private bool _permControl = true;
    [ObservableProperty] private bool _permPtt = true;
    [ObservableProperty] private bool _permConfigWrite;
}
