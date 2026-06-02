namespace Qiwo.Sync.Core;

public sealed class FileSelector
{
    private static readonly string[] IncludedExact =
    {
        "custom_phrase.txt",
        "symbols.yaml"
    };

    private static readonly string[] IncludedExtensions =
    {
        ".custom.yaml",
        ".schema.yaml",
        ".dict.yaml"
    };

    private static readonly string[] IncludedDirectories =
    {
        "opencc/",
        "lua/",
        "sync/"
    };

    private static readonly string[] ExcludedDirectories =
    {
        ".git/",
        ".qiwo-sync/",
        "build/"
    };

    private static readonly string[] ExcludedExtensions =
    {
        ".bin"
    };

    private static readonly string[] ExcludedSuffixes =
    {
        ".table.bin",
        ".reverse.bin",
        ".userdb"
    };

    public bool ShouldSync(string relativePath)
    {
        var path = PathUtil.NormalizeRelativePath(relativePath);
        var lower = path.ToLowerInvariant();

        if (ExcludedDirectories.Any(lower.StartsWith))
        {
            return false;
        }

        if (lower.Split('/').Any(segment => segment.EndsWith(".userdb", StringComparison.Ordinal)))
        {
            return false;
        }

        if (ExcludedSuffixes.Any(lower.EndsWith) || ExcludedExtensions.Any(lower.EndsWith))
        {
            return false;
        }

        if (IncludedExact.Contains(lower, StringComparer.Ordinal))
        {
            return true;
        }

        if (IncludedExtensions.Any(lower.EndsWith))
        {
            return true;
        }

        return IncludedDirectories.Any(lower.StartsWith);
    }
}
