using System.Buffers.Binary;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;
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
    /// Establishes the persistent QUIC connection. Two trust modes, exactly one of
    /// which applies:
    /// <list type="bullet">
    /// <item><paramref name="insecure"/> = <c>true</c> — dev trust-any (accept any
    /// server cert, no CA file needed; pairs with the server's ephemeral-CA auto-gen).</item>
    /// <item><paramref name="insecure"/> = <c>false</c> — prod-like CA pinning:
    /// <paramref name="caCertPath"/> is REQUIRED and the server chain is validated
    /// against ONLY that CA (custom root trust, no system-roots fallback — mirrors the
    /// Rust <c>TrustAnchor</c> with exactly one anchor).</item>
    /// </list>
    /// Either way the client presents NO client certificate (the player plane is
    /// server-cert-only; the caller is authenticated per-call by the bearer token).
    /// </summary>
    public static async Task<QuicPlayerClient> ConnectAsync(
        string host,
        int port,
        string? caCertPath,
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
        {
            ssl.RemoteCertificateValidationCallback = static (_, _, _, _) => true;
        }
        else
        {
            if (string.IsNullOrEmpty(caCertPath))
                throw new ArgumentException(
                    "prod mode requires a CA certificate path (caCertPath); pass insecure:true for dev trust-any.",
                    nameof(caCertPath));
            ssl.RemoteCertificateValidationCallback = PinToCa(caCertPath);
        }
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

    /// <summary>
    /// Builds a server-cert validation callback that pins to exactly the CA loaded
    /// from <paramref name="caCertPath"/> (a PEM CERTIFICATE — the same anchor
    /// <c>edgeca</c> writes and Rust's <c>TrustAnchor::load_cert_only</c> reads). The
    /// server chain must chain to that one root: <see cref="X509ChainTrustMode.CustomRootTrust"/>
    /// with a single custom trust anchor and NO system-roots fallback, revocation
    /// checking off (the ephemeral dev CA publishes no CRL/OCSP).
    /// </summary>
    private static RemoteCertificateValidationCallback PinToCa(string caCertPath)
    {
        // Load the CA cert once (cert-only PEM; no private key — a player holds only
        // the anchor). CreateFromPem(certPem) loads the CERTIFICATE alone; the *FromPemFile
        // and two-arg overloads insist on a matching private key, which an anchor lacks.
        string pem = File.ReadAllText(caCertPath);
        X509Certificate2 caCert = X509Certificate2.CreateFromPem(pem);

        return (_, serverCert, _, _) =>
        {
            if (serverCert is null)
                return false;
            using var leaf = new X509Certificate2(serverCert);
            using var chain = new X509Chain();
            chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
            chain.ChainPolicy.CustomTrustStore.Add(caCert);
            chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
            return chain.Build(leaf);
        };
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
