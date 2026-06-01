namespace Qiwo.Sync.Core;

public sealed class FrostInitializer
{
    private readonly FileSelector _selector = new();

    public async Task<SyncSummary> InitializeAsync(SyncRequest request, CancellationToken cancellationToken)
    {
        if (request.FrostDir is null)
        {
            throw new ArgumentException("FrostDir is required for init-frost.");
        }

        if (!request.FrostDir.Exists)
        {
            throw new DirectoryNotFoundException($"rime-frost directory does not exist: {request.FrostDir.FullName}");
        }

        if (!request.DryRun)
        {
            request.RimeUserDir.Create();
        }

        var messages = new List<string>();
        var copied = 0;
        var skipped = 0;
        var schema = new FileInfo(Path.Combine(request.RimeUserDir.FullName, SyncConstants.FrostSchemaFileName));

        if (!schema.Exists)
        {
            foreach (var source in request.FrostDir.EnumerateFiles("*", SearchOption.AllDirectories))
            {
                var relativePath = PathUtil.ToRelativePath(request.FrostDir, source);
                if (relativePath.StartsWith(".git/", StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }

                if (!IsFrostResource(relativePath))
                {
                    continue;
                }

                var target = new FileInfo(Path.Combine(
                    request.RimeUserDir.FullName,
                    relativePath.Replace('/', Path.DirectorySeparatorChar)));

                if (target.Exists)
                {
                    skipped++;
                    continue;
                }

                if (!request.DryRun)
                {
                    target.Directory?.Create();
                    source.CopyTo(target.FullName);
                }

                copied++;
            }
        }
        else
        {
            messages.Add("rime-frost schema already exists; resource copy skipped.");
        }

        await EnsureDefaultCustomYamlAsync(request, cancellationToken);

        return new SyncSummary
        {
            Mode = SyncMode.InitFrost,
            Frontend = request.Frontend,
            DeviceId = request.DeviceId,
            Downloaded = copied,
            Skipped = skipped,
            Messages = messages
        };
    }

    private async Task EnsureDefaultCustomYamlAsync(SyncRequest request, CancellationToken cancellationToken)
    {
        var file = new FileInfo(Path.Combine(request.RimeUserDir.FullName, SyncConstants.DefaultCustomYaml));
        if (file.Exists && file.Length > 0)
        {
            return;
        }

        const string content =
            """
            patch:
              schema_list:
                - schema: rime_frost
            """;

        if (request.DryRun)
        {
            return;
        }

        file.Directory?.Create();
        await File.WriteAllTextAsync(file.FullName, content + Environment.NewLine, cancellationToken);
    }

    private bool IsFrostResource(string relativePath)
    {
        if (_selector.ShouldSync(relativePath))
        {
            return true;
        }

        var lower = PathUtil.NormalizeRelativePath(relativePath).ToLowerInvariant();
        return lower.EndsWith(".yaml", StringComparison.Ordinal) ||
               lower is "installation.yaml" ||
               lower.StartsWith("cn_dicts/", StringComparison.Ordinal) ||
               lower.StartsWith("en_dicts/", StringComparison.Ordinal) ||
               lower.StartsWith("others/", StringComparison.Ordinal);
    }
}
