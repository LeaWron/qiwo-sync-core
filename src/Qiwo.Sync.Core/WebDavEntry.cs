namespace Qiwo.Sync.Core;

public sealed record WebDavEntry
{
    public required string RelativePath { get; init; }

    public bool IsCollection { get; init; }

    public long? ContentLength { get; init; }

    public DateTimeOffset? LastModifiedUtc { get; init; }

    public string? ETag { get; init; }
}
