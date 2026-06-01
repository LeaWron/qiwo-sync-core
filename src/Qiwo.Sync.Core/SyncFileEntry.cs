namespace Qiwo.Sync.Core;

public sealed record SyncFileEntry
{
    public required string RelativePath { get; init; }

    public required long Size { get; init; }

    public required string Sha256 { get; init; }

    public required DateTimeOffset LastWriteUtc { get; init; }

    public string? ETag { get; init; }
}
