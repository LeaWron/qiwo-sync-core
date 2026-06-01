using System.Text.Json.Serialization;

namespace Qiwo.Sync.Core;

public sealed record SyncManifest
{
    public int Version { get; init; } = 1;

    public string DeviceId { get; init; } = string.Empty;

    public string Frontend { get; init; } = string.Empty;

    public DateTimeOffset UpdatedAtUtc { get; init; } = DateTimeOffset.UtcNow;

    public IReadOnlyDictionary<string, SyncFileEntry> Files { get; init; } =
        new Dictionary<string, SyncFileEntry>(StringComparer.OrdinalIgnoreCase);

    [JsonIgnore]
    public static SyncManifest Empty { get; } = new();
}
