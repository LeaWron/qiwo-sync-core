namespace Qiwo.Sync.Core;

public sealed record SyncSummary
{
    public SyncMode Mode { get; init; }

    public Frontend Frontend { get; init; }

    public string DeviceId { get; init; } = string.Empty;

    public int Uploaded { get; init; }

    public int Downloaded { get; init; }

    public int ConflictsBackedUp { get; init; }

    public int Skipped { get; init; }

    public IReadOnlyList<string> Messages { get; init; } = Array.Empty<string>();
}
