using System;
using System.Diagnostics;
using Avalonia.Controls;
using QsoRipper.Gui.ViewModels;

namespace QsoRipper.Gui.Views;

internal sealed partial class CallsignCardView : UserControl
{
    private CallsignCardViewModel? _attachedViewModel;

    public CallsignCardView()
    {
        InitializeComponent();
        DataContextChanged += OnDataContextChanged;
        DetachedFromVisualTree += (_, _) => Detach();
    }

    private void OnDataContextChanged(object? sender, EventArgs e)
    {
        Detach();
        if (DataContext is CallsignCardViewModel vm)
        {
            _attachedViewModel = vm;
            vm.ExpandMapRequested += OnExpandMapRequested;
            vm.OpenExternalUrlRequested += OnOpenExternalUrlRequested;
        }
    }

    private void Detach()
    {
        if (_attachedViewModel is not null)
        {
            _attachedViewModel.ExpandMapRequested -= OnExpandMapRequested;
            _attachedViewModel.OpenExternalUrlRequested -= OnOpenExternalUrlRequested;
            _attachedViewModel = null;
        }
    }

    private void OnExpandMapRequested(object? sender, EventArgs e)
    {
        if (_attachedViewModel is not { IsMapAvailable: true } vm)
        {
            return;
        }

        var popout = new MapPopoutWindow();
        var subtitle = string.IsNullOrWhiteSpace(vm.MapCountryLabel)
            ? vm.MapDistanceText
            : $"{vm.MapCountryLabel}  ·  {vm.MapDistanceText}";
        var title = string.IsNullOrWhiteSpace(vm.Callsign) ? "Azimuthal Map" : $"Azimuthal Map · {vm.Callsign}";
        popout.Configure(title, subtitle, vm.MapPath, vm.MapScaleKm);

        var owner = TopLevel.GetTopLevel(this) as Window;
        if (owner is not null)
        {
            popout.Show(owner);
        }
        else
        {
            popout.Show();
        }
    }

    private static void OnOpenExternalUrlRequested(object? sender, string url)
    {
        if (!Uri.TryCreate(url, UriKind.Absolute, out var uri))
        {
            return;
        }
        if (!uri.Scheme.Equals(Uri.UriSchemeHttp, StringComparison.OrdinalIgnoreCase)
            && !uri.Scheme.Equals(Uri.UriSchemeHttps, StringComparison.OrdinalIgnoreCase))
        {
            return;
        }

        try
        {
            _ = Process.Start(new ProcessStartInfo(uri.AbsoluteUri)
            {
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            Trace.WriteLine($"[CallsignCard] Failed to open URL '{uri}': {ex.Message}");
        }
    }
}
