using System.Net.Quic;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameBackend.Client.Generated;
using GameBackend.Client.Transport;

// gbclient — the external C# player-plane driver (the QUIC hard-gate verification
// client). Mirrors tools/playercli's exit grammar:
//
//   exit 0  iff the transport call succeeded (ok:true) AND the domain payload
//           status == "Ok";
//   exit 1  reached the server but the outcome was not Ok (transport fault OR a
//           domain non-Ok status — auth failures arrive as ok:true per the pinned
//           error grammar), or a typed-client exception in flow mode;
//   exit 2  usage / argument error;
//   exit 3  QUIC (System.Net.Quic / msquic) unsupported on this platform.
//
// Two modes:
//   gbclient raw  --addr HOST:PORT (--ca PATH | --insecure) [--token TOK] METHOD [JSON]
//   gbclient flow --addr HOST:PORT (--ca PATH | --insecure)
//
// raw drives the untyped IPlayerTransport (can call ANY method string, including
// wire-only ones — for negative scenarios). flow drives the GENERATED typed client
// end-to-end (register → create → list), proving the typed client over pure QUIC.

const int ExitOk = 0;
const int ExitNotOk = 1;
const int ExitUsage = 2;
const int ExitNoQuic = 3;

const string Usage =
    "usage:\n" +
    "  gbclient raw  --addr HOST:PORT (--ca PATH | --insecure) [--token TOK] [--api-key KEY] METHOD [JSON-PAYLOAD]\n" +
    "  gbclient flow --addr HOST:PORT (--ca PATH | --insecure) [--api-key KEY]";

if (args.Length == 0)
{
    Console.Error.WriteLine(Usage);
    return ExitUsage;
}

// QUIC guard up front — this is the SKIP signal the verify stage keys on.
if (!QuicConnection.IsSupported)
{
    Console.Error.WriteLine("QUIC (System.Net.Quic / msquic) is not supported on this platform.");
    return ExitNoQuic;
}

string mode = args[0];
string[] rest = args[1..];

try
{
    return mode switch
    {
        "raw" => await RunRawAsync(rest),
        "flow" => await RunFlowAsync(rest),
        "--help" or "-h" => Help(),
        _ => Fail($"unknown mode {mode.QuoteArg()} (expected 'raw' or 'flow')"),
    };
}
catch (ArgumentException ex)
{
    // Thrown by the parser / connect for bad usage.
    Console.Error.WriteLine($"gbclient: {ex.Message}");
    Console.Error.WriteLine(Usage);
    return ExitUsage;
}
catch (Exception ex)
{
    // Dial / handshake / framing / CA-load fault: a transport failure, not a bad
    // outcome. playercli likewise maps these to exit 1.
    Console.Error.WriteLine($"gbclient: {ex.GetType().Name}: {ex.Message}");
    return ExitNotOk;
}

int Help()
{
    Console.Error.WriteLine(Usage);
    return ExitUsage;
}

int Fail(string msg)
{
    Console.Error.WriteLine($"gbclient: {msg}");
    Console.Error.WriteLine(Usage);
    return ExitUsage;
}

// -------------------------------------------------------------------------------
// raw mode — untyped, one call, playercli-parity exit grammar.
// -------------------------------------------------------------------------------
async Task<int> RunRawAsync(string[] rawArgs)
{
    var opts = CliOptions.Parse(rawArgs, wantMethod: true);

    await using var client = await opts.ConnectAsync();
    Console.Error.WriteLine(
        $"connected: QUIC {opts.Host}:{opts.Port} ALPN={QuicPlayerClient.Alpn} ({opts.TrustLabel})");

    // Default payload is `{}` (NOT null): no-arg methods still decode into an empty
    // request struct on the front, and serde rejects a null there.
    byte[] payload = Encoding.UTF8.GetBytes(opts.Payload ?? "{}");

    PlayerResponse resp = await client.CallAsync(opts.Method!, opts.Token, payload);

    // Print the response payload JSON to stdout so the harness can grep the inner
    // status (Unauthorized / NotFound / …).
    Console.WriteLine(resp.Payload?.ToJsonString() ?? "<no payload>");
    Console.Error.WriteLine($"transport ok={resp.Ok} error={resp.Error ?? "<none>"}");

    if (!resp.Ok)
        return ExitNotOk;

    string? status = resp.Payload?["status"]?.GetValue<string>();
    Console.Error.WriteLine($"domain status={status ?? "<none>"}");
    return status == "Ok" ? ExitOk : ExitNotOk;
}

// -------------------------------------------------------------------------------
// flow mode — the generated TYPED client, end-to-end over pure QUIC.
// -------------------------------------------------------------------------------
async Task<int> RunFlowAsync(string[] flowArgs)
{
    var opts = CliOptions.Parse(flowArgs, wantMethod: false);

    await using var transport = await opts.ConnectAsync();
    Console.Error.WriteLine(
        $"connected: QUIC {opts.Host}:{opts.Port} ALPN={QuicPlayerClient.Alpn} ({opts.TrustLabel})");

    var gb = new GameBackendClient(transport);

    try
    {
        string email = $"csharp-{Guid.NewGuid():N}@test.local";
        Console.Error.WriteLine($"[flow] register {email}");
        Session session = await gb.AccountsRegisterAsync(email, "hunter2-longenough", "C# Flow");
        Console.Error.WriteLine($"[flow] registered player_id={session.PlayerId} token={Truncate(session.Token)}");

        Console.Error.WriteLine("[flow] create character 'hero'");
        Character hero = await gb.CharactersCreateAsync(session.Token, "hero", "");
        Console.Error.WriteLine($"[flow] created character id={hero.Id} name={hero.Name}");

        Console.Error.WriteLine("[flow] list characters");
        Character[] chars = await gb.CharactersListAsync(session.Token);
        Console.Error.WriteLine($"[flow] listed {chars.Length} character(s): {string.Join(", ", chars.Select(c => c.Id))}");

        bool found = chars.Any(c => c.Id == hero.Id);
        if (!found)
        {
            Console.Error.WriteLine($"[flow] ASSERTION FAILED: created id {hero.Id} not in list");
            Console.WriteLine("flow FAILED: created character not present in list");
            return ExitNotOk;
        }

        Console.WriteLine($"flow OK: player={session.PlayerId} character={hero.Id} listed={chars.Length}");
        return ExitOk;
    }
    catch (GameBackendStatusException ex)
    {
        Console.Error.WriteLine($"[flow] status error: {ex.Message}");
        Console.WriteLine($"flow FAILED: {ex.Status}");
        return ExitNotOk;
    }
    catch (GameBackendTransportException ex)
    {
        Console.Error.WriteLine($"[flow] transport error: {ex.Message}");
        Console.WriteLine("flow FAILED: transport fault");
        return ExitNotOk;
    }
}

static string Truncate(string s) => s.Length <= 12 ? s : s[..12] + "…";

// -------------------------------------------------------------------------------
// Hand-rolled arg parsing (playercli style): known `--flag value` pairs, then bare
// tokens are METHOD then [JSON-PAYLOAD].
// -------------------------------------------------------------------------------
sealed class CliOptions
{
    public required string Addr { get; init; }
    public string? CaCertPath { get; init; }
    public bool Insecure { get; init; }
    public string? Token { get; init; }
    public string? ApiKey { get; init; }
    public string? Method { get; init; }
    public string? Payload { get; init; }

    public string Host => SplitAddr().Host;
    public int Port => SplitAddr().Port;
    public string TrustLabel => Insecure ? "trust-any" : $"pinned CA {CaCertPath}";

    private (string Host, int Port) SplitAddr()
    {
        int colon = Addr.LastIndexOf(':');
        if (colon <= 0 || colon == Addr.Length - 1)
            throw new ArgumentException($"bad --addr {Addr.QuoteArg()} (expected HOST:PORT)");
        string host = Addr[..colon];
        if (!int.TryParse(Addr[(colon + 1)..], out int port))
            throw new ArgumentException($"bad --addr {Addr.QuoteArg()} (port not a number)");
        return (host, port);
    }

    public Task<QuicPlayerClient> ConnectAsync() =>
        QuicPlayerClient.ConnectAsync(Host, Port, CaCertPath, Insecure, ApiKey);

    public static CliOptions Parse(string[] args, bool wantMethod)
    {
        string? addr = null;
        string? ca = null;
        bool insecure = false;
        string? token = null;
        string? apiKey = null;
        var positional = new List<string>();

        for (int i = 0; i < args.Length; i++)
        {
            string a = args[i];
            switch (a)
            {
                case "--addr" or "-addr":
                    addr = Next(args, ref i, "--addr");
                    break;
                case "--ca" or "-ca":
                    ca = Next(args, ref i, "--ca");
                    break;
                case "--insecure":
                    insecure = true;
                    break;
                case "--token" or "-token":
                    token = Next(args, ref i, "--token");
                    break;
                case "--api-key" or "-api-key":
                    apiKey = Next(args, ref i, "--api-key");
                    break;
                case "--help" or "-h":
                    throw new ArgumentException("help requested");
                default:
                    positional.Add(a);
                    break;
            }
        }

        if (string.IsNullOrEmpty(addr))
            throw new ArgumentException("--addr HOST:PORT is required");
        if (insecure == (ca is not null))
            throw new ArgumentException("exactly one of --insecure or --ca PATH is required (mutually exclusive)");

        string? method = null;
        string? payload = null;
        if (wantMethod)
        {
            if (positional.Count == 0)
                throw new ArgumentException("a METHOD argument is required");
            method = positional[0];
            payload = positional.Count > 1 ? positional[1] : null;
        }
        else if (positional.Count > 0)
        {
            throw new ArgumentException($"unexpected argument {positional[0].QuoteArg()}");
        }

        return new CliOptions
        {
            Addr = addr,
            CaCertPath = ca,
            Insecure = insecure,
            Token = token,
            ApiKey = apiKey,
            Method = method,
            Payload = payload,
        };
    }

    private static string Next(string[] args, ref int i, string flag)
    {
        if (i + 1 >= args.Length)
            throw new ArgumentException($"{flag} requires a value");
        return args[++i];
    }
}

internal static class ArgExtensions
{
    public static string QuoteArg(this string s) => $"'{s}'";
}
