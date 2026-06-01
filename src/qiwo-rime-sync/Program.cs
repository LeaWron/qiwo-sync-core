using System.Text.Json;
using Qiwo.Sync.Core;

var result = await ProgramMain.RunAsync(args, CancellationToken.None);
Environment.Exit(result);

internal static class ProgramMain
{
    public static async Task<int> RunAsync(string[] args, CancellationToken cancellationToken)
    {
        try
        {
            if (args.Length == 0 || args.Contains("--help", StringComparer.OrdinalIgnoreCase))
            {
                WriteHelp();
                return 0;
            }

            var mode = ParseMode(args[0]);
            var options = ParseOptions(args.Skip(1));
            var request = BuildRequest(mode, options);
            var summary = await new SyncEngine().ExecuteAsync(request, cancellationToken);

            if (options.ContainsKey("json"))
            {
                Console.WriteLine(JsonSerializer.Serialize(summary, SyncJsonContext.Default.SyncSummary));
            }
            else
            {
                foreach (var message in summary.Messages)
                {
                    Console.WriteLine(message);
                }

                Console.WriteLine(
                    $"mode={summary.Mode} uploaded={summary.Uploaded} downloaded={summary.Downloaded} " +
                    $"conflicts={summary.ConflictsBackedUp} skipped={summary.Skipped}");
            }

            return 0;
        }
        catch (Exception ex) when (ex is ArgumentException or DirectoryNotFoundException or InvalidOperationException or HttpRequestException)
        {
            Console.Error.WriteLine(ex.Message);
            return 2;
        }
    }

    private static SyncRequest BuildRequest(SyncMode mode, IReadOnlyDictionary<string, string?> options)
    {
        var frontend = ParseFrontend(GetRequired(options, "frontend"));
        var rimeUserDir = new DirectoryInfo(GetRequired(options, "rime-user-dir"));
        var deviceId = GetOptional(options, "device-id") ?? Environment.MachineName;
        var password = GetOptional(options, "password");
        var passwordEnv = GetOptional(options, "password-env");

        if (!string.IsNullOrWhiteSpace(passwordEnv))
        {
            password = Environment.GetEnvironmentVariable(passwordEnv);
        }

        return new SyncRequest
        {
            Mode = mode,
            Frontend = frontend,
            RimeUserDir = rimeUserDir,
            RemoteUrl = mode == SyncMode.InitFrost ? null : new Uri(GetRequired(options, "remote-url")),
            Username = GetOptional(options, "username"),
            Password = password,
            DeviceId = deviceId,
            FrostDir = GetOptional(options, "frost-dir") is { } frostDir ? new DirectoryInfo(frostDir) : null,
            DryRun = options.ContainsKey("dry-run")
        };
    }

    private static Dictionary<string, string?> ParseOptions(IEnumerable<string> args)
    {
        var parsed = new Dictionary<string, string?>(StringComparer.OrdinalIgnoreCase);
        var values = args.ToArray();

        for (var i = 0; i < values.Length; i++)
        {
            var token = values[i];
            if (!token.StartsWith("--", StringComparison.Ordinal))
            {
                throw new ArgumentException($"Unexpected argument: {token}");
            }

            var key = token[2..];
            if (key is "dry-run" or "json")
            {
                parsed[key] = null;
                continue;
            }

            if (i + 1 >= values.Length || values[i + 1].StartsWith("--", StringComparison.Ordinal))
            {
                throw new ArgumentException($"Missing value for option: {token}");
            }

            parsed[key] = values[++i];
        }

        return parsed;
    }

    private static SyncMode ParseMode(string value)
    {
        return value.ToLowerInvariant() switch
        {
            "sync" => SyncMode.Sync,
            "push" => SyncMode.Push,
            "pull" => SyncMode.Pull,
            "init-frost" => SyncMode.InitFrost,
            _ => throw new ArgumentException($"Unknown mode: {value}")
        };
    }

    private static Frontend ParseFrontend(string value)
    {
        return value.ToLowerInvariant() switch
        {
            "weasel" => Frontend.Weasel,
            "squirrel" => Frontend.Squirrel,
            "ibus-rime" or "ibus" => Frontend.IbusRime,
            "trime" => Frontend.Trime,
            _ => throw new ArgumentException($"Unknown frontend: {value}")
        };
    }

    private static string GetRequired(IReadOnlyDictionary<string, string?> options, string key)
    {
        return GetOptional(options, key) ?? throw new ArgumentException($"Missing required option: --{key}");
    }

    private static string? GetOptional(IReadOnlyDictionary<string, string?> options, string key)
    {
        return options.TryGetValue(key, out var value) && !string.IsNullOrWhiteSpace(value) ? value : null;
    }

    private static void WriteHelp()
    {
        Console.WriteLine(
            """
            qiwo-rime-sync

            Usage:
              qiwo-rime-sync sync|push|pull --frontend weasel --rime-user-dir <dir> --remote-url <url> [options]
              qiwo-rime-sync init-frost --frontend weasel --rime-user-dir <dir> --frost-dir <dir> [options]

            Options:
              --frontend       weasel | squirrel | ibus-rime | trime
              --rime-user-dir  Rime user directory
              --remote-url     WebDAV collection URL
              --username       WebDAV user name
              --password       WebDAV password
              --password-env   Environment variable containing the WebDAV password
              --device-id      Stable device id, defaults to machine name
              --frost-dir      rime-frost repository path for init-frost
              --dry-run        Plan actions without writing files
              --json           Print JSON summary
            """);
    }
}
