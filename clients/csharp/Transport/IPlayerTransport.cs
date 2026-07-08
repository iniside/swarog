using System.Text.Json.Nodes;

namespace GameBackend.Client.Transport;

/// <summary>
/// The decoded player-plane response envelope (edge::wire::Response). <see cref="Ok"/>
/// is the TRANSPORT flag: <c>false</c> means a transport fault (framing / envelope /
/// unwired front) and <see cref="Error"/> carries the reason. A completed operation is
/// always <c>Ok == true</c> with the domain outcome (<c>{status, err, value}</c>) riding
/// inside <see cref="Payload"/> — callers MUST inspect that inner status, not just
/// <see cref="Ok"/> (the pinned error grammar: an auth failure arrives as Ok).
/// </summary>
public sealed record PlayerResponse(bool Ok, JsonNode? Payload, string? Error);

/// <summary>
/// The single seam the generated typed client (Step 3) and the hand-written QUIC
/// transport (Step 4) both agree on — pinned here in Step 1 so neither can drift. One
/// call opens one bidirectional stream over the persistent connection, frames the
/// player envelope <c>{method, token?, payload}</c>, and returns the decoded response.
/// </summary>
public interface IPlayerTransport
{
    /// <param name="method">The wire method string, e.g. <c>"leaderboard.topScores"</c>.</param>
    /// <param name="token">The bearer token for an <c>AuthPlayer</c> op, or <c>null</c>.</param>
    /// <param name="payload">Already-encoded JSON request bytes; empty ⇒ sent as JSON <c>null</c>.</param>
    Task<PlayerResponse> CallAsync(
        string method,
        string? token,
        ReadOnlyMemory<byte> payload,
        CancellationToken ct = default);
}
