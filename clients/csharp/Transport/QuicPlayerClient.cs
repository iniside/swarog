using System.Buffers.Binary;
using System.Net;
using System.Net.Quic;
using System.Net.Security;
using System.Runtime.InteropServices;
using System.Runtime.Versioning;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace GameBackend.Client.Transport;

/// <summary>
/// The player-plane QUIC transport — the C# port of Rust's <c>edge::PlayerClient</c>.
/// One persistent QUIC connection; each call opens a fresh bidirectional stream
/// (persistent conn, stream-per-call). TLS 1.3 with ALPN <c>edge-player</c>, a
/// configurable target host, server-cert-only (no client certificate is presented). Framing is
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
    private readonly string? _apiKey;

    private QuicPlayerClient(QuicConnection conn, string? apiKey)
    {
        _conn = conn;
        _apiKey = apiKey;
    }

    /// <summary>
    /// Establishes the persistent QUIC connection to <paramref name="host"/>:<paramref name="port"/>.
    /// An IP literal is dialed directly; a DNS name is resolved and every address is
    /// dialed in resolver order until one connects (once one does, each earlier failure
    /// is reported to <paramref name="onDialFailure"/>; if none connects, the thrown
    /// <see cref="IOException"/> names every failed address). The client presents NO
    /// client certificate (the player plane is server-cert-only; the caller is
    /// authenticated per-call by the bearer token).
    /// </summary>
    /// <param name="trust">How the server certificate is validated (see <see cref="PlayerTrust"/>).</param>
    /// <param name="targetHost">
    /// The TLS server name (SNI and the name the certificate must carry); <c>null</c>
    /// means <paramref name="host"/> when it is a DNS name, else <c>localhost</c>.
    /// </param>
    /// <param name="apiKey">
    /// The connection-scoped API key (mirrors the Rust gateway's <c>X-Api-Key</c>
    /// policy check), or <c>null</c> to send no key. Stamped into every call's
    /// envelope by <see cref="CallAsync"/> — the key identifies the CLIENT CLASS for
    /// the whole connection, not a single call, so it is bound here rather than
    /// threaded through <see cref="IPlayerTransport.CallAsync"/>.
    /// </param>
    public static async Task<QuicPlayerClient> ConnectAsync(
        string host,
        int port,
        PlayerTrust trust,
        string? targetHost = null,
        string? apiKey = null,
        Action<IPEndPoint, Exception>? onDialFailure = null,
        CancellationToken ct = default)
    {
        if (!QuicConnection.IsSupported)
            throw new PlatformNotSupportedException(
                "System.Net.Quic (msquic) is not available on this platform.");

        bool isIpLiteral = IPAddress.TryParse(host, out IPAddress? literal);
        var ssl = new SslClientAuthenticationOptions
        {
            // Arbitrary ALPN string is allowed (not restricted to h2/h3).
            ApplicationProtocols = new List<SslApplicationProtocol> { new(Alpn) },
            TargetHost = targetHost ?? (isIpLiteral ? "localhost" : host),
        };
        switch (trust)
        {
            case PlayerTrust.Insecure:
                ssl.RemoteCertificateValidationCallback = static (_, _, _, _) => true;
                break;
            case PlayerTrust.PinnedCa pinned:
                if (string.IsNullOrEmpty(pinned.CaCertPath))
                    throw new ArgumentException(
                        "pinned-CA trust requires a CA certificate path.", nameof(trust));
                ssl.RemoteCertificateValidationCallback = PinToCa(pinned.CaCertPath);
                break;
            case PlayerTrust.SystemRoots:
                break;
            default:
                throw new ArgumentException($"unknown trust mode {trust}", nameof(trust));
        }
        // No ClientCertificates: the player plane is server-cert-only.

        IPAddress[] addresses = isIpLiteral
            ? new[] { literal! }
            : await Dns.GetHostAddressesAsync(host, ct).ConfigureAwait(false);
        if (addresses.Length == 0)
            throw new IOException($"dial {host}:{port}: host resolved to no address");

        var failures = new List<(IPEndPoint EndPoint, Exception Error)>();
        QuicConnection? conn = null;
        foreach (IPAddress address in addresses)
        {
            var endPoint = new IPEndPoint(address, port);
            var opts = new QuicClientConnectionOptions
            {
                RemoteEndPoint = endPoint,
                ClientAuthenticationOptions = ssl,
                DefaultStreamErrorCode = 0,
                DefaultCloseErrorCode = 0,
            };
            try
            {
                conn = await QuicConnection.ConnectAsync(opts, ct).ConfigureAwait(false);
                break;
            }
            catch (Exception ex) when (!ct.IsCancellationRequested)
            {
                failures.Add((endPoint, ex));
            }
        }
        if (conn is null)
        {
            string detail = string.Join("; ", failures.Select(f => $"{f.EndPoint}: {f.Error.GetType().Name}: {f.Error.Message}"));
            throw new IOException($"dial {host}:{port}: {detail}", failures[^1].Error);
        }
        try
        {
            foreach (var failure in failures)
                onDialFailure?.Invoke(failure.EndPoint, failure.Error);
            return new QuicPlayerClient(conn, apiKey);
        }
        catch
        {
            await conn.DisposeAsync().ConfigureAwait(false);
            throw;
        }
    }

    /// <summary>
    /// Builds a server-cert validation callback that pins to exactly the CA loaded
    /// from <paramref name="caCertPath"/> (a PEM CERTIFICATE — the anchor
    /// <c>edgeca</c> writes). The server chain must chain to that one root:
    /// <see cref="X509ChainTrustMode.CustomRootTrust"/> with a single custom trust
    /// anchor and NO system-roots fallback, revocation checking off (the ephemeral dev
    /// CA publishes no CRL/OCSP). The chain is built from the intermediates the server
    /// sent and the built chain's root must be byte-equal to the pinned CA. The leaf
    /// must be valid for server authentication (EKU <c>1.3.6.1.5.5.7.3.1</c>). Every
    /// policy error other than a chain error (which the custom chain build replaces)
    /// fails the handshake, so the leaf must also match the target host.
    /// </summary>
    private static RemoteCertificateValidationCallback PinToCa(string caCertPath)
    {
        // Load the CA cert once (cert-only PEM; no private key — a player holds only
        // the anchor). CreateFromPem(certPem) loads the CERTIFICATE alone; the *FromPemFile
        // and two-arg overloads insist on a matching private key, which an anchor lacks.
        string pem = File.ReadAllText(caCertPath);
        X509Certificate2 caCert = X509Certificate2.CreateFromPem(pem);

        return (_, serverCert, presented, sslPolicyErrors) =>
        {
            if (serverCert is null)
                return false;
            if ((sslPolicyErrors & ~SslPolicyErrors.RemoteCertificateChainErrors) != SslPolicyErrors.None)
                return false;
            using var leaf = new X509Certificate2(serverCert);
            using var chain = new X509Chain();
            chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
            chain.ChainPolicy.CustomTrustStore.Add(caCert);
            chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
            chain.ChainPolicy.ApplicationPolicy.Add(new Oid("1.3.6.1.5.5.7.3.1"));
            if (presented is not null)
                chain.ChainPolicy.ExtraStore.AddRange(presented.ChainPolicy.ExtraStore);
            if (OperatingSystem.IsWindows())
                chain.ChainPolicy.ExtraStore.AddRange(CertificateContextStore(leaf));
            if (!chain.Build(leaf))
                return false;
            X509Certificate2 root = chain.ChainElements[chain.ChainElements.Count - 1].Certificate;
            return root.RawDataMemory.Span.SequenceEqual(caCert.RawDataMemory.Span);
        };
    }

    /// <summary>
    /// The certificates in the store that <paramref name="certificate"/>'s Windows
    /// <c>CERT_CONTEXT</c> belongs to (empty when it has none).
    /// </summary>
    [SupportedOSPlatform("windows")]
    private static X509Certificate2Collection CertificateContextStore(X509Certificate2 certificate)
    {
        IntPtr storeHandle = Marshal.ReadIntPtr(certificate.Handle, CertContextStoreOffset);
        if (storeHandle == IntPtr.Zero)
            return new X509Certificate2Collection();
        using var store = new X509Store(storeHandle);
        return store.Certificates;
    }

    /// <summary>The layout of the Windows <c>CERT_CONTEXT</c> struct (wincrypt.h).</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct CertContext
    {
        public uint CertEncodingType;
        public IntPtr CertEncoded;
        public uint CertEncodedLength;
        public IntPtr CertInfo;
        public IntPtr CertStore;
    }

    private static readonly int CertContextStoreOffset =
        (int)Marshal.OffsetOf<CertContext>(nameof(CertContext.CertStore));

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
        if (_apiKey is not null)
            env["api_key"] = _apiKey;

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
