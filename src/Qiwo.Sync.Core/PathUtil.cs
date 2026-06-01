namespace Qiwo.Sync.Core;

internal static class PathUtil
{
    public static string NormalizeRelativePath(string path)
    {
        return path.Replace('\\', '/').TrimStart('/');
    }

    public static string ToRelativePath(DirectoryInfo root, FileInfo file)
    {
        var relative = Path.GetRelativePath(root.FullName, file.FullName);
        return NormalizeRelativePath(relative);
    }

    public static string CombineRemotePath(Uri baseUri, string relativePath)
    {
        var root = baseUri.ToString().TrimEnd('/');
        var encoded = string.Join(
            "/",
            NormalizeRelativePath(relativePath)
                .Split('/', StringSplitOptions.RemoveEmptyEntries)
                .Select(Uri.EscapeDataString));

        return $"{root}/{encoded}";
    }
}
