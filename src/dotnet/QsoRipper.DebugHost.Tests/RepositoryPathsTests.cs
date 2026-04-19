using QsoRipper.DebugHost.Services;

namespace QsoRipper.DebugHost.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public class RepositoryPathsTests
{
    [Fact]
    public void Derives_expected_repo_paths_from_content_root()
    {
        var root = Path.GetFullPath(Path.Combine(Path.GetTempPath(), "fakerepo"));
        var contentRoot = Path.Combine(root, "src", "dotnet", "QsoRipper.DebugHost");

        var paths = new RepositoryPaths(contentRoot);

        Assert.Equal(root, paths.RepoRoot);
        Assert.Equal(Path.Combine(root, "src", "rust"), paths.RustWorkspaceRoot);
        Assert.Equal(Path.Combine(root, "src", "rust", "Cargo.toml"), paths.RustWorkspaceManifestPath);
        Assert.Equal(Path.Combine(root, "src", "dotnet", "QsoRipper.slnx"), paths.DotnetWorkspaceSolutionPath);
        Assert.Equal(Path.Combine(root, "tests", "Run-EngineConformance.ps1"), paths.EngineConformanceScriptPath);
        Assert.Equal(Path.Combine("src", "rust", "Cargo.toml"), paths.GetRepoRelativePath(paths.RustWorkspaceManifestPath));
    }
}
#pragma warning restore CA1707
