using System.Buffers.Binary;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace GameBackend.Client.Transport;

/// <summary>
/// The player-plane QUIC transport — the C# port of Rust's <c>edge::PlayerClient</c>.
/// One persistent QUIC connection; each call opens a fresh bidirectional stream
/// (persistent conn, stream-per-call). TLS 1.3 with ALPN <c>edge-player</c>, SNI
/// <c>localhost</c>, server-cert-only (no client certificate is presented). Framing is
/// a 4-byte big-endian length prefix + JSON body, one framed request/response per
/// stream.
/// </summary>
public sealed class QuicPlayerClient : IPlayerTransport, IAsyncDisposable
{
    /// <summary>The player-plane ALPN id (must equal <c>edge::PLAYER_ALPN</c>).</summary>
    public const string Alpn = "edge-player";

    /// <summary>Mirrors <c>edge::MAX_PLAYER_FRAME</c> (1 MiB) — reject a hostile length prefix.</summary>
    private const int MaxFrame = 1 << 20;

    private readonly QuicConnection _conn;

    private QuicPlayerClient(QuicConnection conn) => _conn = conn;

    /// <summary>
    /// Establishes the persistent QUIC connection. <paramref name="insecure"/> = dev
    /// trust-any (accept any server cert, no CA file needed — pairs with the server's
    /// ephemeral-CA auto-gen). The prod-like CA-pinning path is added in Step 4.
    /// </summary>
    public static async Task<QuicPlayerClient> ConnectAsync(
        string host,
        int port,
        bool insecure,
        CancellationToken ct = default)
    {
        if (!QuicConnection.IsSupported)
            throw new PlatformNotSupportedException(
                "System.Net.Quic (msquic) is not available on this platform.");

        var ssl = new SslClientAuthenticationOptions
        {
            // Arbitrary ALPN string is allowed (not restricted to h2/h3).
            ApplicationProtocols = new List<SslApplicationProtocol> { new(Alpn) },
            // The server leaf carries a `localhost` SAN + loopback IPs; dial by name.
            TargetHost = "localhost",
        };
        if (insecure)
            ssl.RemoteCertificateValidationCallback = static (_, _, _, _) => true;
        // No ClientCertificates: the player plane is server-cert-only.

        var opts = new QuicClientConnectionOptions
        {
            RemoteEndPoint = new IPEndPoint(IPAddress.Parse(host), port),
            ClientAuthenticationOptions = ssl,
            DefaultStreamErrorCode = 0,
            DefaultCloseErrorCode = 0,
        };

        var conn = await QuicConnection.ConnectAsync(opts, ct).ConfigureAwait(false);
        return new QuicPlayerClient(conn);
    }

    /// <inheritdoc/>
    public async Task<PlayerResponse> CallAsync(
        string method,
        string? token,
        ReadOnlyMemory<byte> payload,
        CancellationToken ct = default)
    {
        // Build the player envelope. An empty payload is sent as JSON `null` (matches
        // Rust `raw_from_bytes`: empty ⇒ "null"); a non-empty payload is spliced in
        // verbatim as raw JSON, never re-encoded.
        JsonNode? payloadNode = payload.IsEmpty ? null : JsonNode.Parse(payload.Span);
        var env = new JsonObject
        {
            ["method"] = method,
            ["payload"] = payloadNode,
        };
        if (token is not null)
            env["token"] = token;

        byte[] envBytes = JsonSerializer.SerializeToUtf8Bytes(env);
        if (envBytes.Length > MaxFrame)
            throw new InvalidOperationException(
                $"request frame {envBytes.Length} exceeds MAX_PLAYER_FRAME {MaxFrame}");

        // One contiguous frame: 4-byte BE length + body, written in a single write.
        byte[] frame = new byte[4 + envBytes.Length];
        BinaryPrimitives.WriteUInt32BigEndian(frame, (uint)envBytes.Length);
        envBytes.CopyTo(frame, 4);

        await using QuicStream stream =
            await _conn.OpenOutboundStreamAsync(QuicStreamType.Bidirectional, ct).ConfigureAwait(false);
        // completeWrites: signal EOF on the send side so the server reads the full
        // frame then stops — the stream IS the request/response correlation.
        await stream.WriteAsync(frame, completeWrites: true, ct).ConfigureAwait(false);

        byte[] lenBuf = await ReadExactAsync(stream, 4, ct).ConfigureAwait(false);
        uint respLen = BinaryPrimitives.ReadUInt32BigEndian(lenBuf);
        if (respLen > MaxFrame)
            throw new InvalidOperationException(
                $"response frame {respLen} exceeds MAX_PLAYER_FRAME {MaxFrame}");

        byte[] respBytes = await ReadExactAsync(stream, (int)respLen, ct).ConfigureAwait(false);
        JsonNode root = JsonNode.Parse(respBytes)
            ?? throw new InvalidOperationException("response envelope is JSON null");

        bool ok = root["ok"]?.GetValue<bool>() ?? false;
        JsonNode? respPayload = root["payload"];
        string? error = root["error"]?.GetValue<string>();
        return new PlayerResponse(ok, respPayload, error);
    }

    /// <summary>Reads exactly <paramref name="n"/> bytes or throws on early EOF.</summary>
    private static async Task<byte[]> ReadExactAsync(QuicStream stream, int n, CancellationToken ct)
    {
        byte[] buf = new byte[n];
        int off = 0;
        while (off < n)
        {
            int r = await stream.ReadAsync(buf.AsMemory(off, n - off), ct).ConfigureAwait(false);
            if (r == 0)
                throw new EndOfStreamException($"stream closed after {off}/{n} bytes");
            off += r;
        }
        return buf;
    }

    public async ValueTask DisposeAsync()
    {
        await _conn.DisposeAsync().ConfigureAwait(false);
    }
}
