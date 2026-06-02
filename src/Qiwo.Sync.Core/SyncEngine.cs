namespace Qiwo.Sync.Core;

public sealed class SyncEngine
{
    private readonly FileSelector _selector = new();

    public async Task<SyncSummary> ExecuteAsync(SyncRequest request, CancellationToken cancellationToken = default)
    {
        if (request.Mode == SyncMode.InitFrost)
        {
            return await new FrostInitializer().InitializeAsync(request, cancellationToken);
        }

        if (request.RemoteUrl is null)
        {
            throw new ArgumentException("RemoteUrl is required for WebDAV sync.");
        }

        if (!request.DryRun)
        {
            request.RimeUserDir.Create();
        }

        using var webDav = new WebDavClient(request.RemoteUrl, request.Username, request.Password);
        if (!request.DryRun)
        {
            await webDav.EnsureRootAsync(cancellationToken);
        }

        return request.Mode switch
        {
            SyncMode.Push => await PushAsync(request, webDav, cancellationToken),
            SyncMode.Pull => await PullAsync(request, webDav, cancellationToken),
            SyncMode.Sync => await SyncAsync(request, webDav, cancellationToken),
            SyncMode.SyncUserDict => await SyncUserDictAsync(request, webDav, cancellationToken),
            _ => throw new NotSupportedException($"Unsupported mode: {request.Mode}")
        };
    }

    private async Task<SyncSummary> PushAsync(
        SyncRequest request,
        WebDavClient webDav,
        CancellationToken cancellationToken)
    {
        var local = new LocalFileStore(request.RimeUserDir, _selector);
        var localFiles = local.Scan();
        var messages = new List<string>();
        var uploaded = 0;

        foreach (var entry in localFiles.Values.OrderBy(entry => entry.RelativePath, StringComparer.OrdinalIgnoreCase))
        {
            if (!request.DryRun)
            {
                await webDav.PutFileAsync(entry.RelativePath, local.ResolveFile(entry.RelativePath), cancellationToken);
            }

            uploaded++;
        }

        var manifest = CreateManifest(request, localFiles);
        await WriteManifestsAsync(request, webDav, manifest, cancellationToken);

        messages.Add($"Pushed {uploaded} file(s).");
        return CreateSummary(request, uploaded: uploaded, messages: messages);
    }

    private async Task<SyncSummary> PullAsync(
        SyncRequest request,
        WebDavClient webDav,
        CancellationToken cancellationToken)
    {
        var local = new LocalFileStore(request.RimeUserDir, _selector);
        var remoteManifest = await ReadRemoteManifestAsync(webDav, cancellationToken);
        var downloaded = 0;
        var skipped = 0;
        var messages = new List<string>();

        foreach (var entry in remoteManifest.Files.Values.OrderBy(entry => entry.RelativePath, StringComparer.OrdinalIgnoreCase))
        {
            if (!_selector.ShouldSync(entry.RelativePath))
            {
                skipped++;
                continue;
            }

            if (!request.DryRun)
            {
                await webDav.DownloadFileAsync(entry.RelativePath, local.ResolveFile(entry.RelativePath), cancellationToken);
            }

            downloaded++;
        }

        var localManifest = CreateManifest(request, !request.DryRun ? local.Scan() : remoteManifest.Files);
        if (!request.DryRun)
        {
            await new ManifestStore(request.RimeUserDir).WriteLocalAsync(localManifest, cancellationToken);
        }

        messages.Add($"Pulled {downloaded} file(s).");
        return CreateSummary(request, downloaded: downloaded, skipped: skipped, messages: messages);
    }

    private async Task<SyncSummary> SyncAsync(
        SyncRequest request,
        WebDavClient webDav,
        CancellationToken cancellationToken)
    {
        var local = new LocalFileStore(request.RimeUserDir, _selector);
        var manifestStore = new ManifestStore(request.RimeUserDir);
        var previousManifest = await manifestStore.ReadLocalAsync(cancellationToken);
        var remoteManifest = await ReadRemoteManifestAsync(webDav, cancellationToken);
        var localFiles = local.Scan();
        var uploaded = 0;
        var downloaded = 0;
        var skipped = 0;
        var conflicts = 0;
        var messages = new List<string>();

        var allPaths = localFiles.Keys
            .Concat(remoteManifest.Files.Keys)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        foreach (var path in allPaths)
        {
            if (!_selector.ShouldSync(path))
            {
                skipped++;
                continue;
            }

            localFiles.TryGetValue(path, out var localEntry);
            remoteManifest.Files.TryGetValue(path, out var remoteEntry);
            previousManifest.Files.TryGetValue(path, out var previousEntry);

            if (localEntry is not null && remoteEntry is not null && localEntry.Sha256 == remoteEntry.Sha256)
            {
                skipped++;
                continue;
            }

            var localChanged = localEntry is not null &&
                (previousEntry is null || localEntry.Sha256 != previousEntry.Sha256);
            var remoteChanged = remoteEntry is not null &&
                (previousEntry is null || remoteEntry.Sha256 != previousEntry.Sha256);

            if (localEntry is not null && remoteEntry is null)
            {
                await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                uploaded++;
                continue;
            }

            if (localEntry is null && remoteEntry is not null)
            {
                await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                downloaded++;
                continue;
            }

            if (localEntry is null || remoteEntry is null)
            {
                skipped++;
                continue;
            }

            if (localChanged && !remoteChanged)
            {
                await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                uploaded++;
                continue;
            }

            if (!localChanged && remoteChanged)
            {
                await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                downloaded++;
                continue;
            }

            if (localChanged && remoteChanged)
            {
                await DownloadAsync(request, webDav, local, remoteEntry, backup: true, cancellationToken);
                downloaded++;
                conflicts++;
                messages.Add($"Conflict backed up, remote kept: {path}");
                continue;
            }

            var localWins = localEntry.LastWriteUtc >= remoteEntry.LastWriteUtc;
            if (localWins)
            {
                await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                uploaded++;
            }
            else
            {
                await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                downloaded++;
            }
        }

        var finalFiles = request.DryRun ? localFiles : local.Scan();
        var finalManifest = CreateManifest(request, finalFiles);
        await WriteManifestsAsync(request, webDav, finalManifest, cancellationToken);

        messages.Add($"Uploaded {uploaded}, downloaded {downloaded}, conflicts {conflicts}.");
        return CreateSummary(
            request,
            uploaded: uploaded,
            downloaded: downloaded,
            conflicts: conflicts,
            skipped: skipped,
            messages: messages);
    }

    /// <summary>
    /// 仅同步用户词库文本导出（sync/ 目录）。
    /// 平台代码应在调用此方法前先触发 Rime 的 sync_user_data() 导出词库。
    /// </summary>
    private async Task<SyncSummary> SyncUserDictAsync(
        SyncRequest request,
        WebDavClient webDav,
        CancellationToken cancellationToken)
    {
        var local = new LocalFileStore(request.RimeUserDir, _selector);
        var manifestStore = new ManifestStore(request.RimeUserDir);
        var previousManifest = await manifestStore.ReadLocalAsync(cancellationToken);
        var remoteManifest = await ReadRemoteManifestAsync(webDav, cancellationToken);
        var localFiles = local.Scan();

        // 过滤：仅保留 sync/ 目录下的文件（用户词库文本导出）
        var localDictFiles = new Dictionary<string, SyncFileEntry>(StringComparer.OrdinalIgnoreCase);
        foreach (var kv in localFiles)
        {
            if (kv.Key.StartsWith("sync/", StringComparison.OrdinalIgnoreCase))
                localDictFiles[kv.Key] = kv.Value;
        }

        var remoteDictFiles = new Dictionary<string, SyncFileEntry>(StringComparer.OrdinalIgnoreCase);
        foreach (var kv in remoteManifest.Files)
        {
            if (kv.Key.StartsWith("sync/", StringComparison.OrdinalIgnoreCase))
                remoteDictFiles[kv.Key] = kv.Value;
        }

        var uploaded = 0;
        var downloaded = 0;
        var skipped = 0;
        var conflicts = 0;
        var messages = new List<string>();

        var allPaths = localDictFiles.Keys
            .Concat(remoteDictFiles.Keys)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .OrderBy(path => path, StringComparer.OrdinalIgnoreCase);

        foreach (var path in allPaths)
        {
            if (!_selector.ShouldSync(path))
            {
                skipped++;
                continue;
            }

            localDictFiles.TryGetValue(path, out var localEntry);
            remoteDictFiles.TryGetValue(path, out var remoteEntry);
            previousManifest.Files.TryGetValue(path, out var previousEntry);

            // 文件相同 → 跳过
            if (localEntry is not null && remoteEntry is not null
                && string.Equals(localEntry.Sha256, remoteEntry.Sha256, StringComparison.OrdinalIgnoreCase))
            {
                skipped++;
                continue;
            }

            var localChanged = localEntry is not null
                && (previousEntry is null
                    || !string.Equals(localEntry.Sha256, previousEntry.Sha256, StringComparison.OrdinalIgnoreCase));
            var remoteChanged = remoteEntry is not null
                && (previousEntry is null
                    || !string.Equals(remoteEntry.Sha256, previousEntry.Sha256, StringComparison.OrdinalIgnoreCase));

            if (localEntry is not null && remoteEntry is null)
            {
                await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                uploaded++;
            }
            else if (localEntry is null && remoteEntry is not null)
            {
                await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                downloaded++;
            }
            else if (localEntry is not null && remoteEntry is not null)
            {
                if (localChanged && !remoteChanged)
                {
                    await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                    uploaded++;
                }
                else if (!localChanged && remoteChanged)
                {
                    await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                    downloaded++;
                }
                else if (localChanged && remoteChanged)
                {
                    await DownloadAsync(request, webDav, local, remoteEntry, backup: true, cancellationToken);
                    downloaded++;
                    conflicts++;
                    messages.Add($"Conflict backed up, remote kept: {path}");
                }
                else
                {
                    var localWins = localEntry.LastWriteUtc >= remoteEntry.LastWriteUtc;
                    if (localWins)
                    {
                        await UploadAsync(request, webDav, local, localEntry, cancellationToken);
                        uploaded++;
                    }
                    else
                    {
                        await DownloadAsync(request, webDav, local, remoteEntry, backup: false, cancellationToken);
                        downloaded++;
                    }
                }
            }
            else
            {
                skipped++;
            }
        }

        var finalFiles = request.DryRun ? localFiles : local.Scan();
        var finalManifest = CreateManifest(request, finalFiles);
        await WriteManifestsAsync(request, webDav, finalManifest, cancellationToken);

        messages.Add($"Dict sync — uploaded {uploaded}, downloaded {downloaded}, conflicts {conflicts}.");
        return CreateSummary(
            request,
            uploaded: uploaded,
            downloaded: downloaded,
            conflicts: conflicts,
            skipped: skipped,
            messages: messages);
    }

    private static async Task UploadAsync(
        SyncRequest request,
        WebDavClient webDav,
        LocalFileStore local,
        SyncFileEntry entry,
        CancellationToken cancellationToken)
    {
        if (!request.DryRun)
        {
            await webDav.PutFileAsync(entry.RelativePath, local.ResolveFile(entry.RelativePath), cancellationToken);
        }
    }

    private static async Task DownloadAsync(
        SyncRequest request,
        WebDavClient webDav,
        LocalFileStore local,
        SyncFileEntry entry,
        bool backup,
        CancellationToken cancellationToken)
    {
        if (request.DryRun)
        {
            return;
        }

        if (backup)
        {
            local.Backup(entry.RelativePath, DateTimeOffset.UtcNow);
        }

        await webDav.DownloadFileAsync(entry.RelativePath, local.ResolveFile(entry.RelativePath), cancellationToken);
    }

    private static SyncManifest CreateManifest(
        SyncRequest request,
        IReadOnlyDictionary<string, SyncFileEntry> files)
    {
        return new SyncManifest
        {
            DeviceId = request.DeviceId,
            Frontend = request.Frontend.ToString(),
            UpdatedAtUtc = DateTimeOffset.UtcNow,
            Files = new Dictionary<string, SyncFileEntry>(files, StringComparer.OrdinalIgnoreCase)
        };
    }

    private static async Task<SyncManifest> ReadRemoteManifestAsync(
        WebDavClient webDav,
        CancellationToken cancellationToken)
    {
        var bytes = await webDav.GetBytesAsync(SyncConstants.RemoteManifestFileName, cancellationToken);
        return bytes is null ? SyncManifest.Empty : ManifestStore.FromJsonBytes(bytes);
    }

    private static async Task WriteManifestsAsync(
        SyncRequest request,
        WebDavClient webDav,
        SyncManifest manifest,
        CancellationToken cancellationToken)
    {
        if (request.DryRun)
        {
            return;
        }

        await new ManifestStore(request.RimeUserDir).WriteLocalAsync(manifest, cancellationToken);
        await webDav.PutBytesAsync(
            SyncConstants.RemoteManifestFileName,
            ManifestStore.ToJsonBytes(manifest),
            cancellationToken);
    }

    private static SyncSummary CreateSummary(
        SyncRequest request,
        int uploaded = 0,
        int downloaded = 0,
        int conflicts = 0,
        int skipped = 0,
        IReadOnlyList<string>? messages = null)
    {
        return new SyncSummary
        {
            Mode = request.Mode,
            Frontend = request.Frontend,
            DeviceId = request.DeviceId,
            Uploaded = uploaded,
            Downloaded = downloaded,
            ConflictsBackedUp = conflicts,
            Skipped = skipped,
            Messages = messages ?? Array.Empty<string>()
        };
    }
}
