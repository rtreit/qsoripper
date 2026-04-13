using Avalonia.Controls;
using Avalonia.Input;

namespace QsoRipper.Gui.Views;

internal sealed partial class MainWindow : Window
{
    private readonly MenuItem? _fileMenuItem;

    public MainWindow()
    {
        InitializeComponent();
        _fileMenuItem = this.FindControl<MenuItem>("FileMenuItem");
        KeyDown += OnWindowKeyDown;
    }

    protected override async void OnOpened(System.EventArgs e)
    {
        base.OnOpened(e);
        if (DataContext is ViewModels.MainWindowViewModel vm)
        {
            await vm.CheckFirstRunAsync();
        }
    }

    private void OnWindowKeyDown(object? sender, KeyEventArgs e)
    {
        if (e.KeyModifiers == KeyModifiers.Alt && e.Key == Key.F && _fileMenuItem is not null)
        {
            _fileMenuItem.Focus();
            _fileMenuItem.IsSubMenuOpen = true;
            e.Handled = true;
        }
    }
}
