using System.Net.Quic;
using GameBackend.Client.Transport;

// Step-1 hard-gate spike: prove a C#/.NET client can establish the player-QUIC
// connection and invoke ONE method end-to-end. Target: `leaderboard.topScores`
// (auth=none → no token, the simplest reachable call). Later steps replace this
// entrypoint with the raw/flow CLI (Step 4).
//
//   gbclient [host] [port]     (defaults 127.0.0.1 9100)
//
// Exit: 0 = transport ok AND inner status "Ok"; 1 = reached server but not Ok;
//       3 = QUIC unsupported on this platform.

string host = args.Length > 0 ? args[0] : "127.0.0.1";
int port = args.Length > 1 ? int.Parse(args[1]) : 9100;

if (!QuicConnection.IsSupported)
{
    Console.Error.WriteLine("QUIC (System.Net.Quic / msquic) is not supported on this platform.");
    return 3;
}

try
{
    await using var client = await QuicPlayerClient.ConnectAsync(host, port, insecure: true);
    Console.Error.WriteLine($"connected: QUIC {host}:{port} ALPN={QuicPlayerClient.Alpn} (trust-any)");

    // No-arg methods still decode into an (empty) request struct on the front, so the
    // payload must be `{}` — NOT `null` (serde rejects null → struct). This is the
    // rule the generated typed client will follow: always serialize a request DTO.
    byte[] emptyObj = "{}"u8.ToArray();
    PlayerResponse resp = await client.CallAsync("leaderboard.topScores", token: null, payload: emptyObj);

    Console.WriteLine(resp.Payload?.ToJsonString() ?? "<no payload>");
    Console.Error.WriteLine($"transport ok={resp.Ok} error={resp.Error ?? "<none>"}");

    if (!resp.Ok)
        return 1;

    string? status = resp.Payload?["status"]?.GetValue<string>();
    Console.Error.WriteLine($"domain status={status ?? "<none>"}");
    return status == "Ok" ? 0 : 1;
}
catch (Exception ex)
{
    Console.Error.WriteLine($"FAILED: {ex.GetType().Name}: {ex.Message}");
    return 1;
}
