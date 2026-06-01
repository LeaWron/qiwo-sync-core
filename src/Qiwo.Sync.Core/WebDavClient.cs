using System.Net;
using System.Net.Http.Headers;
using System.Text;
using System.Xml.Linq;

namespace Qiwo.Sync.Core;

public sealed class WebDavClient : IDisposable
{
    private readonly HttpClient _httpClient;
    private readonly Uri _root;

    public WebDavClient(Uri root, string? username, string? password)
    {
        _root = EnsureCollectionUri(root);
        _httpClient = new HttpClient();

        if (!string.IsNullOrEmpty(username))
        {
            var token = Convert.ToBase64String(Encoding.UTF8.GetBytes($"{username}:{password ?? string.Empty}"));
            _httpClient.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Basic", token);
        }
    }

    public async Task EnsureRootAsync(CancellationToken cancellationToken)
    {
        await EnsureCollectionAsync(string.Empty, cancellationToken);
    }

    public async Task EnsureCollectionAsync(string relativePath, CancellationToken cancellationToken)
    {
        var segments = PathUtil.NormalizeRelativePath(relativePath)
            .Split('/', StringSplitOptions.RemoveEmptyEntries);

        var current = string.Empty;
        foreach (var segment in segments)
        {
            current = string.IsNullOrEmpty(current) ? segment : $"{current}/{segment}";
            await MkcolAsync(current, cancellationToken);
        }

        if (segments.Length == 0)
        {
            await MkcolAsync(string.Empty, cancellationToken);
        }
    }

    public async Task<IReadOnlyDictionary<string, WebDavEntry>> PropfindAsync(CancellationToken cancellationToken)
    {
        using var request = new HttpRequestMessage(new HttpMethod("PROPFIND"), _root);
        request.Headers.Add("Depth", "infinity");

        const string body =
            """
            <?xml version="1.0" encoding="utf-8" ?>
            <D:propfind xmlns:D="DAV:">
              <D:prop>
                <D:getetag/>
                <D:getlastmodified/>
                <D:getcontentlength/>
                <D:resourcetype/>
              </D:prop>
            </D:propfind>
            """;
        request.Content = new StringContent(body, Encoding.UTF8, "application/xml");

        using var response = await _httpClient.SendAsync(request, cancellationToken);
        if (response.StatusCode == HttpStatusCode.NotFound)
        {
            return new Dictionary<string, WebDavEntry>(StringComparer.OrdinalIgnoreCase);
        }

        response.EnsureSuccessStatusCode();
        var xml = await response.Content.ReadAsStringAsync(cancellationToken);
        return ParsePropfind(xml);
    }

    public async Task PutFileAsync(string relativePath, FileInfo file, CancellationToken cancellationToken)
    {
        var directory = Path.GetDirectoryName(PathUtil.NormalizeRelativePath(relativePath))?.Replace('\\', '/');
        if (!string.IsNullOrEmpty(directory))
        {
            await EnsureCollectionAsync(directory, cancellationToken);
        }

        await using var stream = file.OpenRead();
        using var content = new StreamContent(stream);
        using var response = await _httpClient.PutAsync(PathUtil.CombineRemotePath(_root, relativePath), content, cancellationToken);
        response.EnsureSuccessStatusCode();
    }

    public async Task PutBytesAsync(string relativePath, byte[] bytes, CancellationToken cancellationToken)
    {
        using var content = new ByteArrayContent(bytes);
        using var response = await _httpClient.PutAsync(PathUtil.CombineRemotePath(_root, relativePath), content, cancellationToken);
        response.EnsureSuccessStatusCode();
    }

    public async Task<byte[]?> GetBytesAsync(string relativePath, CancellationToken cancellationToken)
    {
        using var response = await _httpClient.GetAsync(PathUtil.CombineRemotePath(_root, relativePath), cancellationToken);
        if (response.StatusCode == HttpStatusCode.NotFound)
        {
            return null;
        }

        response.EnsureSuccessStatusCode();
        return await response.Content.ReadAsByteArrayAsync(cancellationToken);
    }

    public async Task DownloadFileAsync(string relativePath, FileInfo target, CancellationToken cancellationToken)
    {
        using var response = await _httpClient.GetAsync(PathUtil.CombineRemotePath(_root, relativePath), cancellationToken);
        response.EnsureSuccessStatusCode();

        target.Directory?.Create();
        await using var input = await response.Content.ReadAsStreamAsync(cancellationToken);
        await using var output = File.Create(target.FullName);
        await input.CopyToAsync(output, cancellationToken);
    }

    public void Dispose()
    {
        _httpClient.Dispose();
    }

    private async Task MkcolAsync(string relativePath, CancellationToken cancellationToken)
    {
        var uri = string.IsNullOrEmpty(relativePath)
            ? _root
            : new Uri(PathUtil.CombineRemotePath(_root, relativePath).TrimEnd('/') + "/");

        using var request = new HttpRequestMessage(new HttpMethod("MKCOL"), uri);
        using var response = await _httpClient.SendAsync(request, cancellationToken);

        if (response.StatusCode is HttpStatusCode.MethodNotAllowed or HttpStatusCode.Conflict)
        {
            return;
        }

        if (response.StatusCode == HttpStatusCode.Created)
        {
            return;
        }

        if (response.StatusCode == HttpStatusCode.OK)
        {
            return;
        }

        response.EnsureSuccessStatusCode();
    }

    private IReadOnlyDictionary<string, WebDavEntry> ParsePropfind(string xml)
    {
        var result = new Dictionary<string, WebDavEntry>(StringComparer.OrdinalIgnoreCase);
        var document = XDocument.Parse(xml);
        var dav = XNamespace.Get("DAV:");
        var rootPath = _root.AbsolutePath.TrimEnd('/') + "/";

        foreach (var response in document.Descendants(dav + "response"))
        {
            var href = response.Element(dav + "href")?.Value;
            if (string.IsNullOrWhiteSpace(href))
            {
                continue;
            }

            var path = Uri.UnescapeDataString(new Uri(_root, href).AbsolutePath);
            if (!path.StartsWith(rootPath, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var relativePath = path[rootPath.Length..].Trim('/');
            if (string.IsNullOrEmpty(relativePath))
            {
                continue;
            }

            var prop = response.Descendants(dav + "prop").FirstOrDefault();
            if (prop is null)
            {
                continue;
            }

            var isCollection = prop.Element(dav + "resourcetype")?.Element(dav + "collection") is not null;
            var lengthText = prop.Element(dav + "getcontentlength")?.Value;
            var lastModifiedText = prop.Element(dav + "getlastmodified")?.Value;

            result[PathUtil.NormalizeRelativePath(relativePath)] = new WebDavEntry
            {
                RelativePath = PathUtil.NormalizeRelativePath(relativePath),
                IsCollection = isCollection,
                ContentLength = long.TryParse(lengthText, out var length) ? length : null,
                LastModifiedUtc = DateTimeOffset.TryParse(lastModifiedText, out var lastModified)
                    ? lastModified.ToUniversalTime()
                    : null,
                ETag = prop.Element(dav + "getetag")?.Value?.Trim('"')
            };
        }

        return result;
    }

    private static Uri EnsureCollectionUri(Uri uri)
    {
        var text = uri.ToString();
        return text.EndsWith("/", StringComparison.Ordinal) ? uri : new Uri(text + "/");
    }
}
