namespace GameBackend.Client.Transport;

/// <summary>
/// How <see cref="QuicPlayerClient"/> validates the player front's server certificate.
/// Exactly one mode applies per connection; the client presents no client certificate
/// in any of them.
/// </summary>
public abstract record PlayerTrust
{
    private PlayerTrust() { }

    /// <summary>
    /// Dev trust-any: every server certificate is accepted, no CA file needed. Pairs
    /// with the server's ephemeral-CA auto-generation.
    /// </summary>
    public sealed record Insecure : PlayerTrust;

    /// <summary>
    /// The server chain must chain to exactly the CA certificate at
    /// <paramref name="CaCertPath"/> (no system-roots fallback), and the leaf must
    /// match the connection's target host. Mirrors Rust's <c>DevCA::load_cert_only</c>
    /// anchor.
    /// </summary>
    public sealed record PinnedCa(string CaCertPath) : PlayerTrust;

    /// <summary>
    /// The operating system's trust store: .NET validates the chain and the target
    /// host itself. Mirrors Rust's <c>TrustAnchor::system_roots</c>.
    /// </summary>
    public sealed record SystemRoots : PlayerTrust;
}
