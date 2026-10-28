// Bind only the clock in the offline build. The lease body remains unchanged.
using System;
using System.IO;
using System.Linq;
using Newtonsoft.Json.Linq;
public static class LeaseClock
{
    public static DateTime UtcNow = new DateTime(2026, 10, 21, 12, 0, 0, DateTimeKind.Utc);
}
public partial class Context
{
    public JObject Lease(JObject request)
    {
        iRequest = request.ToString();
        oJson = "";
        RunLease();
        return JObject.Parse(oJson);
    }
}
public static class LeaseChecks
{
    static int count;
    static void Check(bool value, string name)
    {
        count++;
        if (!value)
            throw new Exception(name);
    }
    public static void Run()
    {
        var a = new Context();
        var b = new Context();
        b.Session.UserID = "another-user";
        var run = Guid.NewGuid().ToString();
        var other = Guid.NewGuid().ToString();
        var host = Guid.NewGuid();
        var plugin = Guid.NewGuid();
        Func<Guid, string, JObject> resource = (id, role) => new JObject
        {
            ["libraryId"] = "H" + id.ToString("N").Substring(0, 29),
            ["globalId"] = id.ToString(),
            ["role"] = role
        };
        Func<string, string, JObject> request = (action, id) => new JObject
        {
            ["action"] = action,
            ["operationId"] = id
        };
        var begin = request("acquire", run);
        begin["resources"] = new JArray(resource(host, "host"), resource(plugin, "plugin"));
        Check((bool)a.Lease(begin)["accepted"], "acquire");
        var shared = request("acquire", other);
        shared["resources"] = new JArray(resource(host, "host"));
        Check((bool)b.Lease(shared)["accepted"], "another caller shares host");
        LeaseClock.UtcNow = LeaseClock.UtcNow.AddMinutes(4);
        Check((bool)b.Lease(request("renew", other))["accepted"], "renew shared host");
        LeaseClock.UtcNow = LeaseClock.UtcNow.AddMinutes(1);
        Check(!(bool)a.Lease(request("renew", run))["accepted"], "expiry at five minutes");
        bool expiredWork = false;
        try
        {
            a.Execute(new JObject { ["action"] = "begin", ["runId"] = run, ["operationId"] = Guid.NewGuid().ToString() });
        }
        catch (Ice.BLException) { expiredWork = true; }
        Check(expiredWork, "expired caller cannot create staging");

        var claims = (JArray)b.Lease(request("claim", other))["resources"];
        Check(claims.Count == 1 && (string)claims[0]["globalId"] == plugin.ToString(), "active host survives expired plugin");
        var staging = Path.Combine(Sandbox.Root, "Herbert", Guid.Parse(run).ToString("N"), Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(staging);
        var inputId = Guid.NewGuid().ToString("N") + ".input";
        File.WriteAllText(Path.Combine(staging, "state.json"), new JObject
        {
            ["runId"] = run,
            ["operationId"] = Guid.Parse(Path.GetFileName(staging)).ToString(),
            ["company"] = a.Session.CompanyID,
            ["caller"] = a.Session.UserID,
            ["inputs"] = new JArray(new JObject { ["fileId"] = inputId })
        }.ToString());
        File.WriteAllText(Path.Combine(staging, inputId), "fixture");
        var reap = new JObject { ["action"] = "reap", ["globalId"] = plugin.ToString(), ["claimId"] = claims[0]["claimId"] };
        using (var writer = File.Open(Path.Combine(staging, "writer.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None))
        {
            bool busy = false;
            try
            {
                b.Lease(reap);
            }
            catch (IOException) { busy = true; }
            Check(busy && File.Exists(Path.Combine(staging, inputId)), "active writer prevents staging removal");
        }
        File.WriteAllText(Path.Combine(staging, "unknown.txt"), "preserve");
        bool unknown = false;
        try
        {
            b.Lease(reap);
        }
        catch (Ice.BLException) { unknown = true; }
        Check(unknown && File.Exists(Path.Combine(staging, inputId)), "unknown file preserves staging");
        File.Delete(Path.Combine(staging, "unknown.txt"));
        Check((bool)b.Lease(reap)["reaped"] && !Directory.Exists(staging), "another caller removes exact expired staging");
        Directory.CreateDirectory(staging);
        File.WriteAllText(Path.Combine(staging, "writer.lock"), "");
        Check((bool)b.Lease(reap)["reaped"] && !Directory.Exists(staging), "cleanup resumes after state removal");
        Check(((JArray)a.Lease(request("claim", run))["resources"]).Count == 0, "claim excludes another cleaner");
        var complete = new JObject { ["action"] = "complete", ["globalId"] = plugin.ToString(), ["claimId"] = claims[0]["claimId"] };
        Check((bool)a.Lease(complete)["completed"], "different caller completes cleanup");
        Check((bool)b.Lease(request("release", other))["accepted"], "release");
        claims = (JArray)a.Lease(request("claim", run))["resources"];
        Check(claims.Count == 1 && (string)claims[0]["globalId"] == host.ToString(), "host reclaimed without extra timeout");
        var oldToken = (string)claims[0]["claimId"];
        LeaseClock.UtcNow = LeaseClock.UtcNow.AddMinutes(5);
        claims = (JArray)b.Lease(request("claim", other))["resources"];
        Check(claims.Count == 1 && (string)claims[0]["claimId"] != oldToken, "abandoned cleanup can be reclaimed");
        bool rejected = false;
        try
        {
            a.Lease(new JObject { ["action"] = "complete", ["globalId"] = host.ToString(), ["claimId"] = oldToken });
        }
        catch (Ice.BLException) { rejected = true; }
        Check(rejected, "stale cleanup cannot acknowledge a new claim");
        Check(!(bool)a.Lease(request("renew", run))["accepted"], "no revival after cleanup");
        Check((bool)b.Lease(new JObject { ["action"] = "complete", ["globalId"] = host.ToString(), ["claimId"] = claims[0]["claimId"] })["completed"], "host completion");
        Check(((JArray)a.Lease(request("inspect", run))["resources"]).Count == 0, "registry has no resources");
        var orphan = Guid.NewGuid();
        var observe = new JObject { ["action"] = "observe", ["resources"] = new JArray(resource(orphan, "plugin")) };
        Check((bool)a.Lease(observe)["observed"], "server observation needs no client journal");
        LeaseClock.UtcNow = LeaseClock.UtcNow.AddMinutes(4);
        b.Lease(observe);
        Check(((JArray)a.Lease(request("claim", run))["resources"]).Count == 0, "first observation starts grace period");
        LeaseClock.UtcNow = LeaseClock.UtcNow.AddMinutes(1);
        var orphanClaims = (JArray)b.Lease(request("claim", run))["resources"];
        Check(orphanClaims.Count == 1, "repeat discovery does not extend inactivity timeout");
        b.Lease(new JObject { ["action"] = "complete", ["globalId"] = orphan.ToString(), ["claimId"] = orphanClaims[0]["claimId"] });
        var lockPath = Path.Combine(Sandbox.Root, "Herbert/leases-v1/registry.lock");
        using (var guard = File.Open(lockPath, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
        {
            Check((bool)a.Lease(request("inspect", run))["leaseBusy"],
                "independent request reports contention without entering the registry");
        }
        Console.WriteLine("Server lease assertions: " + count);
    }
}
