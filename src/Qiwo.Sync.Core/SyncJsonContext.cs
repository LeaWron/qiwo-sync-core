using System.Text.Json;
using System.Text.Json.Serialization;

namespace Qiwo.Sync.Core;

[JsonSourceGenerationOptions(JsonSerializerDefaults.Web, WriteIndented = true)]
[JsonSerializable(typeof(SyncManifest))]
[JsonSerializable(typeof(SyncSummary))]
public partial class SyncJsonContext : JsonSerializerContext
{
}
