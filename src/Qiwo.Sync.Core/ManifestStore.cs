using System.Text.Json;

namespace Qiwo.Sync.Core;

public sealed class ManifestStore
{
    private readonly DirectoryInfo _root;

    public ManifestStore(DirectoryInfo root)
    {
        _root = root;
    }

    public FileInfo LocalManifestFile =>
        new(Path.Combine(_root.FullName, SyncConstants.StateDirectoryName, SyncConstants.ManifestFileName));

    public async Task<SyncManifest> ReadLocalAsync(CancellationToken cancellationToken)
    {
        var file = LocalManifestFile;
        if (!file.Exists)
        {
            return SyncManifest.Empty;
        }

        await using var stream = file.OpenRead();
        return await JsonSerializer.DeserializeAsync(stream, SyncJsonContext.Default.SyncManifest, cancellationToken)
            ?? SyncManifest.Empty;
    }

    public async Task WriteLocalAsync(SyncManifest manifest, CancellationToken cancellationToken)
    {
        var file = LocalManifestFile;
        file.Directory?.Create();

        await using var stream = File.Create(file.FullName);
        await JsonSerializer.SerializeAsync(stream, manifest, SyncJsonContext.Default.SyncManifest, cancellationToken);
    }

    public static byte[] ToJsonBytes(SyncManifest manifest)
    {
        return JsonSerializer.SerializeToUtf8Bytes(manifest, SyncJsonContext.Default.SyncManifest);
    }

    public static SyncManifest FromJsonBytes(byte[] bytes)
    {
        return JsonSerializer.Deserialize(bytes, SyncJsonContext.Default.SyncManifest) ?? SyncManifest.Empty;
    }
}
