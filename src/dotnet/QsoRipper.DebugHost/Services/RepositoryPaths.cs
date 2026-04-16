using Microsoft.Extensions.Hosting;

namespace QsoRipper.DebugHost.Services;

internal sealed class RepositoryPaths
{
    public RepositoryPaths(IHostEnvironment hostEnvironment)
        : this(hostEnvironment.ContentRootPath)
    {
    }

    public RepositoryPaths(string contentRootPath)
    {
        ArgumentNullException.ThrowIfNull(contentRootPath);

        ContentRoot = Path.GetFullPath(contentRootPath);
        RepoRoot = Path.GetFullPath(Path.Combine(ContentRoot, "..", "..", ".."));
        RustWorkspaceRoot = Path.Combine(RepoRoot, "src", "rust");
        RustWorkspaceManifestPath = Path.Combine(RustWorkspaceRoot, "Cargo.toml");
        DotnetWorkspaceSolutionPath = Path.Combine(RepoRoot, "src", "dotnet", "QsoRipper.slnx");
        EngineConformanceScriptPath = Path.Combine(RepoRoot, "tests", "Run-EngineConformance.ps1");
    }

    public string ContentRoot { get; }

    public string RepoRoot { get; }

    public string RustWorkspaceRoot { get; }

    public string RustWorkspaceManifestPath { get; }

    public string DotnetWorkspaceSolutionPath { get; }

    public string EngineConformanceScriptPath { get; }

    public string GetRepoRelativePath(string absolutePath)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(absolutePath);
        return Path.GetRelativePath(RepoRoot, absolutePath);
    }
}
