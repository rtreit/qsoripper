using System.Globalization;

namespace CatHubFrequencyProbe;

internal sealed class DiagnosticLog
{
    private const int MaxUiLines = 18;
    private readonly Queue<string> _recentLines = new();
    private readonly string _path;

    public DiagnosticLog()
    {
        var directory = System.IO.Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "qsoripper");
        Directory.CreateDirectory(directory);
        _path = System.IO.Path.Combine(directory, "cathub-frequency-probe.log");
    }

    public string Path => _path;

    public string UiText => string.Join(Environment.NewLine, _recentLines);

    public void Write(string message)
    {
        var line = string.Create(
            CultureInfo.InvariantCulture,
            $"{DateTimeOffset.Now:HH:mm:ss.fff} {message}");
        File.AppendAllText(_path, line + Environment.NewLine);
        _recentLines.Enqueue(line);
        while (_recentLines.Count > MaxUiLines)
        {
            _recentLines.Dequeue();
        }
    }
}
