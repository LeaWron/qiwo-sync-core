namespace Qiwo.Sync.Core;

public sealed record SyncRequest
{
    public required Frontend Frontend { get; init; }

    public required DirectoryInfo RimeUserDir { get; init; }

    public Uri? RemoteUrl { get; init; }

    public string? Username { get; init; }

    public string? Password { get; init; }

    public required string DeviceId { get; init; }

    public required SyncMode Mode { get; init; }

    public DirectoryInfo? FrostDir { get; init; }

    public bool DryRun { get; init; }
}
