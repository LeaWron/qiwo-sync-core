using System.Security.Cryptography;

namespace Qiwo.Sync.Core;

public sealed class LocalFileStore
{
    private readonly DirectoryInfo _root;
    private readonly FileSelector _selector;

    public LocalFileStore(DirectoryInfo root, FileSelector selector)
    {
        _root = root;
        _selector = selector;
    }

    public IReadOnlyDictionary<string, SyncFileEntry> Scan()
    {
        if (!_root.Exists)
        {
            return new Dictionary<string, SyncFileEntry>(StringComparer.OrdinalIgnoreCase);
        }

        var entries = new Dictionary<string, SyncFileEntry>(StringComparer.OrdinalIgnoreCase);
        foreach (var file in _root.EnumerateFiles("*", SearchOption.AllDirectories))
        {
            var relativePath = PathUtil.ToRelativePath(_root, file);
            if (!_selector.ShouldSync(relativePath))
            {
                continue;
            }

            entries[relativePath] = CreateEntry(relativePath, file);
        }

        return entries;
    }

    public FileInfo ResolveFile(string relativePath)
    {
        var normalized = PathUtil.NormalizeRelativePath(relativePath);
        var fullPath = Path.GetFullPath(Path.Combine(_root.FullName, normalized.Replace('/', Path.DirectorySeparatorChar)));
        var rootPath = Path.GetFullPath(_root.FullName);
        if (!rootPath.EndsWith(Path.DirectorySeparatorChar))
        {
            rootPath += Path.DirectorySeparatorChar;
        }

        if (!fullPath.StartsWith(rootPath, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException($"Path escapes Rime user directory: {relativePath}");
        }

        return new FileInfo(fullPath);
    }

    public SyncFileEntry CreateEntry(string relativePath)
    {
        return CreateEntry(PathUtil.NormalizeRelativePath(relativePath), ResolveFile(relativePath));
    }

    public void Backup(string relativePath, DateTimeOffset timestamp)
    {
        var source = ResolveFile(relativePath);
        if (!source.Exists)
        {
            return;
        }

        var stamp = timestamp.UtcDateTime.ToString("yyyyMMddHHmmss");
        var target = ResolveFile($"{SyncConstants.StateDirectoryName}/{SyncConstants.BackupDirectoryName}/{stamp}/{relativePath}");
        target.Directory?.Create();
        source.CopyTo(target.FullName, overwrite: true);
    }

    private static SyncFileEntry CreateEntry(string relativePath, FileInfo file)
    {
        using var stream = file.OpenRead();
        var hash = Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();

        return new SyncFileEntry
        {
            RelativePath = relativePath,
            Size = file.Length,
            Sha256 = hash,
            LastWriteUtc = file.LastWriteTimeUtc
        };
    }
}
