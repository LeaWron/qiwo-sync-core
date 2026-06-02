using System.Text;

namespace Qiwo.Sync.Core;

/// <summary>
/// 确保 Rime 的 installation.yaml 包含正确的同步配置。
/// installation_id 使用设备标识（与 WebDAV 同步配置中的 deviceId 一致），
/// sync_dir 指向词库导出目录，供 Rime 原生 sync_user_data() 机制使用。
/// </summary>
public static class InstallationHelper
{
    private const string SyncDir = "sync";

    /// <summary>
    /// 确保 installation.yaml 存在且包含正确的 installation_id 和 sync_dir。
    /// 如果文件已存在且配置正确，不做修改。
    /// </summary>
    public static void Ensure(DirectoryInfo rimeUserDir, string deviceId)
    {
        var file = new FileInfo(Path.Combine(rimeUserDir.FullName, "installation.yaml"));
        file.Directory?.Create();

        // 如果已存在，只更新缺失的字段
        if (file.Exists)
        {
            var existing = File.ReadAllText(file.FullName, Encoding.UTF8);
            var needsUpdate = false;

            // 确保 sync_dir 存在
            if (!existing.Contains("sync_dir:"))
            {
                existing = existing.TrimEnd() + Environment.NewLine;
                existing += $"sync_dir: \"{SyncDir}\"" + Environment.NewLine;
                needsUpdate = true;
            }

            // 确保 installation_id 存在
            if (!existing.Contains("installation_id:"))
            {
                existing = existing.TrimEnd() + Environment.NewLine;
                existing += $"installation_id: \"{MakeSafeId(deviceId)}\"" + Environment.NewLine;
                needsUpdate = true;
            }

            if (needsUpdate)
            {
                File.WriteAllText(file.FullName, existing, Encoding.UTF8);
            }

            return;
        }

        // 新建文件
        var yaml = new StringBuilder();
        yaml.AppendLine($"distribution: \"Qiwo\"");
        yaml.AppendLine($"distribution_version: \"1.0\"");
        yaml.AppendLine($"installation_id: \"{MakeSafeId(deviceId)}\"");
        yaml.AppendLine($"sync_dir: \"{SyncDir}\"");

        File.WriteAllText(file.FullName, yaml.ToString(), Encoding.UTF8);
    }

    /// <summary>
    /// 确保 sync/{deviceId}/ 目录存在，Rime sync_user_data() 会将词库导出到这里。
    /// </summary>
    public static DirectoryInfo EnsureSyncExportDir(DirectoryInfo rimeUserDir, string deviceId)
    {
        var dir = new DirectoryInfo(Path.Combine(rimeUserDir.FullName, SyncDir, MakeSafeId(deviceId)));
        dir.Create();
        return dir;
    }

    private static string MakeSafeId(string deviceId)
    {
        return deviceId
            .Replace(' ', '-')
            .Replace(':', '-')
            .Replace('\\', '-')
            .Replace('/', '-')
            .ToLowerInvariant();
    }
}
