// Shared helper activity. Caller identity does not decide cleanup eligibility.
// Five minutes and thirty seconds are Kevin's selected operating defaults.
var request = Newtonsoft.Json.Linq.JObject.Parse(iRequest);
var action = request.Value<string>("action");
var root = new Ice.Customization.Sandbox.FilePath(
    Ice.Customization.Sandbox.ServerFolder.CompanyData, "Herbert/leases-v1");
Func<string, Ice.Customization.Sandbox.FilePath> path =
    leaf => new Ice.Customization.Sandbox.FilePath(root, leaf);
Sandbox.IO.Directory.CreateDirectory(root);
// Keep this lock file stable. Replacing or deleting it permits two lock owners.
System.IO.Stream registryGuard;
try
{
    registryGuard = Sandbox.IO.File.Open(path("registry.lock"), System.IO.FileMode.OpenOrCreate,
        System.IO.FileAccess.ReadWrite, System.IO.FileShare.None);
}
catch (System.Exception error)
{
    // Sharing conflicts occur during concurrent requests. No state changed.
    var current = error;
    var sharing = false;
    var types = new Newtonsoft.Json.Linq.JArray();
    while (current != null)
    {
        types.Add(new Newtonsoft.Json.Linq.JObject { ["type"] = current.GetType().FullName, ["hresult"] = current.HResult });
        sharing |= current is System.IO.IOException &&
            // Native Linux qualification returns EAGAIN (11); Windows returns
            // sharing/lock violation (32/33). Other IO failures are not retried.
            (current.HResult == 11 || (current.HResult & 0xffff) == 32 || (current.HResult & 0xffff) == 33);
        current = current.InnerException;
    }
    if (!sharing)
    {
        oJson = new Newtonsoft.Json.Linq.JObject { ["leaseAdmissionError"] = types }.ToString();
        return;
    }
    oJson = "{\"leaseBusy\":true}";
    return;
}
using (var guard = registryGuard)
{
    var now = DateTime.UtcNow;
    var registry = Sandbox.IO.File.Exists(path("registry.json"))
        ? Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(path("registry.json")))
        : new Newtonsoft.Json.Linq.JObject
        {
            ["version"] = 1,
            ["operations"] = new Newtonsoft.Json.Linq.JObject(),
            ["resources"] = new Newtonsoft.Json.Linq.JObject()
        };
    if (registry.Value<int>("version") != 1 ||
        !(registry["operations"] is Newtonsoft.Json.Linq.JObject) ||
        !(registry["resources"] is Newtonsoft.Json.Linq.JObject))
        throw new Ice.BLException("The server lease registry is invalid.");
    var operations = (Newtonsoft.Json.Linq.JObject)registry["operations"];
    var resources = (Newtonsoft.Json.Linq.JObject)registry["resources"];
    Action save = () =>
    {
        Sandbox.IO.File.WriteAllText(path("registry.pending"), registry.ToString(Newtonsoft.Json.Formatting.None));
        // Readers use the same lock. Native replacement keeps the prior complete
        // registry if writing the candidate fails.
        Sandbox.IO.File.Move(path("registry.pending"), path("registry.json"), true);
    };
    Func<Newtonsoft.Json.Linq.JToken, bool> active = op =>
        (string)op["state"] == "active" && (long)op["expiresUtcTicks"] > now.Ticks;
    var reply = new Newtonsoft.Json.Linq.JObject { ["serverUtcTicks"] = now.Ticks };
    if (action == "acquire" || action == "renew" || action == "release")
    {
        Guid operation;
        if (!Guid.TryParseExact(request.Value<string>("operationId"), "D", out operation) || operation == Guid.Empty)
            throw new Ice.BLException("A canonical operation ID is required.");
        var id = operation.ToString("D");
        var op = operations[id] as Newtonsoft.Json.Linq.JObject;
        if (action == "acquire")
        {
            var supplied = request["resources"] as Newtonsoft.Json.Linq.JArray;
            if (supplied == null || supplied.Count == 0)
                throw new Ice.BLException("Supply the temporary helpers used by this operation.");
            if (op != null && !active(op))
                throw new Ice.BLException("The operation lease expired. Start a new operation.");
            var keys = op == null ? new Newtonsoft.Json.Linq.JArray() : (Newtonsoft.Json.Linq.JArray)op["resources"].DeepClone();
            foreach (var item in supplied)
            {
                Guid generation;
                var library = (string)item["libraryId"];
                var role = (string)item["role"];
                if (!Guid.TryParseExact((string)item["globalId"], "D", out generation) || generation == Guid.Empty ||
                    library != "H" + generation.ToString("N").Substring(0, 29) ||
                    (role != "host" && role != "plugin"))
                    throw new Ice.BLException("A temporary helper identity and role are required.");
                var key = generation.ToString("D");
                var existing = resources[key];
                if (existing != null && ((string)existing["libraryId"] != library ||
                    (string)existing["role"] != role || (string)existing["state"] != "active"))
                    throw new Ice.BLException("The helper is claimed for cleanup or its identity changed.");
                resources[key] = new Newtonsoft.Json.Linq.JObject
                {
                    ["libraryId"] = library,
                    ["globalId"] = key,
                    ["role"] = role,
                    ["state"] = "active"
                };
                if (!keys.Any(k => (string)k == key))
                    keys.Add(key);
            }
            op = new Newtonsoft.Json.Linq.JObject { ["resources"] = keys, ["state"] = "active" };
            operations[id] = op;
        }
        else if (op == null || !active(op))
        {
            reply["accepted"] = false;
            reply["reason"] = "expired";
            oJson = reply.ToString(Newtonsoft.Json.Formatting.None);
            return;
        }
        // A failed candidate validation above changes only the in-memory copy.
        op["expiresUtcTicks"] = action == "release" ? now.Ticks : now.AddMinutes(5).Ticks;
        op["heartbeatUtcTicks"] = now.Ticks;
        if (action == "release")
            op["state"] = "released";
        save();
        reply["accepted"] = true;
        reply["expiresUtcTicks"] = op["expiresUtcTicks"];
    }
    else if (action == "observe")
    {
        var supplied = request["resources"] as Newtonsoft.Json.Linq.JArray;
        if (supplied == null)
            throw new Ice.BLException("Supply the discovered helper identities.");
        foreach (var item in supplied)
        {
            Guid generation;
            var library = (string)item["libraryId"];
            var role = (string)item["role"];
            if (!Guid.TryParseExact((string)item["globalId"], "D", out generation) || generation == Guid.Empty ||
                library != "H" + generation.ToString("N").Substring(0, 29) ||
                (role != "host" && role != "plugin"))
                throw new Ice.BLException("A temporary helper identity and role are required.");
            var key = generation.ToString("D");
            if (resources[key] != null)
                continue;
            // No heartbeat exists for this older or interrupted installation.
            // Start its five-minute observation window once, not on every scan.
            resources[key] = new Newtonsoft.Json.Linq.JObject
            {
                ["libraryId"] = library,
                ["globalId"] = key,
                ["role"] = role,
                ["state"] = "active",
                ["inactiveAfterUtcTicks"] = now.AddMinutes(5).Ticks
            };
        }
        save();
        reply["observed"] = true;
    }
    else if (action == "inspect" || action == "claim")
    {
        var result = new Newtonsoft.Json.Linq.JArray();
        foreach (var item in resources.Properties())
        {
            if (action == "claim" && request["globalId"] != null &&
                request.Value<string>("globalId") != item.Name)
                continue;
            var resource = (Newtonsoft.Json.Linq.JObject)item.Value;
            var inUse = operations.Properties().Any(p => active(p.Value) &&
                p.Value["resources"].Any(k => (string)k == item.Name));
            inUse |= resource.Value<long?>("inactiveAfterUtcTicks") > now.Ticks;
            if (action == "claim")
            {
                if (inUse || ((string)resource["state"] == "claimed" &&
                    (long)resource["claimExpiresUtcTicks"] > now.Ticks))
                    continue;
                resource["state"] = "claimed";
                resource["claimId"] = Guid.NewGuid().ToString("D");
                resource["claimExpiresUtcTicks"] = now.AddMinutes(5).Ticks;
            }
            var view = (Newtonsoft.Json.Linq.JObject)resource.DeepClone();
            view["active"] = inUse;
            view["runs"] = new Newtonsoft.Json.Linq.JArray(operations.Properties()
                .Where(p => p.Value["resources"].Any(k => (string)k == item.Name)).Select(p => p.Name));
            result.Add(view);
        }
        if (action == "claim")
            save();
        reply["resources"] = result;
    }
    else if (action == "reap")
    {
        var key = request.Value<string>("globalId");
        var resource = key == null ? null : resources[key];
        if (resource == null || (string)resource["state"] != "claimed" ||
            (string)resource["claimId"] != request.Value<string>("claimId"))
            throw new Ice.BLException("The cleanup claim no longer matches.");
        foreach (var op in operations.Properties().Where(p => p.Value["resources"].Any(k => (string)k == key)))
        {
            if (active(op.Value))
                throw new Ice.BLException("The operation is still active.");
            Guid run;
            if (!Guid.TryParseExact(op.Name, "D", out run))
                throw new Ice.BLException("The recorded run ID is invalid.");
            var runRoot = new Ice.Customization.Sandbox.FilePath(
                Ice.Customization.Sandbox.ServerFolder.CompanyData, "Herbert/" + run.ToString("N"));
            if (!Sandbox.IO.Directory.Exists(runRoot))
                continue;
            foreach (var directory in Sandbox.IO.Directory.GetDirectories(runRoot))
            {
                Guid transfer;
                var name = directory.Subpath.Replace('\\', '/').Split('/').Last();
                if (!Guid.TryParseExact(name, "N", out transfer))
                    throw new Ice.BLException("An unknown directory remains in the expired run.");
                Func<string, Ice.Customization.Sandbox.FilePath> staged = leaf => new Ice.Customization.Sandbox.FilePath(directory, leaf);
                using (var writer = Sandbox.IO.File.Open(staged("writer.lock"), System.IO.FileMode.OpenOrCreate,
                    System.IO.FileAccess.ReadWrite, System.IO.FileShare.None))
                {
                    if (!Sandbox.IO.File.Exists(staged("state.json")))
                    {
                        // A previous cleanup can stop after deleting its state.
                        // Only an empty directory and this stable lock may remain.
                        if (Sandbox.IO.Directory.GetDirectories(directory).Length != 0 ||
                            Sandbox.IO.Directory.GetFiles(directory).Any(f =>
                                f.Subpath.Replace('\\', '/').Split('/').Last() != "writer.lock"))
                            throw new Ice.BLException("The staging record is absent; unknown files were preserved.");
                    }
                    else
                    {
                        var state = Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(staged("state.json")));
                        if ((string)state["runId"] != run.ToString("D") ||
                            (string)state["operationId"] != transfer.ToString("D") || (string)state["company"] != Session.CompanyID ||
                            !(state["inputs"] is Newtonsoft.Json.Linq.JArray))
                            throw new Ice.BLException("The staging record does not match the expired operation.");
                        var allowed = new HashSet<string>(StringComparer.Ordinal) { "state.json", "writer.lock", "archive.zip", "producer.started" };
                        foreach (var input in state["inputs"])
                        {
                            var leaf = (string)input["fileId"];
                            Guid file;
                            if (leaf == null || !leaf.EndsWith(".input") ||
                                !Guid.TryParseExact(leaf.Substring(0, leaf.Length - 6), "N", out file) ||
                                leaf != file.ToString("N") + ".input")
                                throw new Ice.BLException("The expired operation has an invalid file ID.");
                            allowed.Add(leaf);
                            foreach (var suffix in new[] { ".receipt.json", ".ready", ".failed", ".started" })
                                allowed.Add(leaf + suffix);
                        }
                        var files = Sandbox.IO.Directory.GetFiles(directory);
                        if (Sandbox.IO.Directory.GetDirectories(directory).Length != 0 ||
                            files.Any(f => !allowed.Contains(f.Subpath.Replace('\\', '/').Split('/').Last())))
                            throw new Ice.BLException("Unknown staging files were preserved.");
                        foreach (var file in files)
                        {
                            var leaf = file.Subpath.Replace('\\', '/').Split('/').Last();
                            if (leaf != "writer.lock" && leaf != "state.json")
                                Sandbox.IO.File.Delete(file);
                        }
                        Sandbox.IO.File.Delete(staged("state.json"));
                    }
                }
                Sandbox.IO.File.Delete(staged("writer.lock"));
                Sandbox.IO.Directory.Delete(directory, false);
            }
            if (Sandbox.IO.Directory.GetFiles(runRoot).Length == 0)
                Sandbox.IO.Directory.Delete(runRoot, false);
        }
        reply["reaped"] = true;
    }
    else if (action == "complete")
    {
        var key = request.Value<string>("globalId");
        var resource = key == null ? null : resources[key];
        if (resource == null || (string)resource["state"] != "claimed" ||
            (string)resource["claimId"] != request.Value<string>("claimId"))
            throw new Ice.BLException("The cleanup claim no longer matches.");
        resources.Remove(key);
        // Completion is sent only after native absence is verified. Remove empty
        // operation receipts; renewal of a missing operation remains refused.
        foreach (var property in operations.Properties().ToArray())
        {
            var keys = (Newtonsoft.Json.Linq.JArray)property.Value["resources"];
            foreach (var item in keys.Where(k => (string)k == key).ToArray())
                item.Remove();
            if (keys.Count == 0)
                operations.Remove(property.Name);
        }
        save();
        reply["completed"] = true;
    }
    else
        throw new Ice.BLException("Unknown server lease operation.");
    oJson = reply.ToString(Newtonsoft.Json.Formatting.None);
}
