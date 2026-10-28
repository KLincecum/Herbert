// Generic run-owned file packaging. No plugin tables or native exporters belong here.
var request = Newtonsoft.Json.Linq.JObject.Parse(iRequest);
var action = request.Value<string>("action");
Guid runId, operationId;
if (!Guid.TryParse(request.Value<string>("runId"), out runId) ||
    !Guid.TryParse(request.Value<string>("operationId"), out operationId) ||
    runId == Guid.Empty || operationId == Guid.Empty)
    throw new Ice.BLException("A run ID and operation ID are required.");
// Admit work under the same server lock used by renewal and cleanup.
// Release it only after the staging lock is held. Cleanup can then observe
// expiry, but it cannot remove files while this request still uses them.
var leaseRoot = new Ice.Customization.Sandbox.FilePath(
    Ice.Customization.Sandbox.ServerFolder.CompanyData, "Herbert/leases-v1");
System.IO.Stream leaseGuard;
try
{
    leaseGuard = Sandbox.IO.File.Open(new Ice.Customization.Sandbox.FilePath(leaseRoot, "registry.lock"),
        System.IO.FileMode.Open, System.IO.FileAccess.ReadWrite, System.IO.FileShare.None);
}
catch (System.Exception error)
{
    // No staging work has started. The caller can retry this admission.
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
try
{
    var registry = Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(
        new Ice.Customization.Sandbox.FilePath(leaseRoot, "registry.json")));
    var lease = registry["operations"]?[runId.ToString("D")];
    if (lease == null || (string)lease["state"] != "active" ||
        (long)lease["expiresUtcTicks"] <= DateTime.UtcNow.Ticks)
        throw new Ice.BLException("The server lease expired. Start a new run.");
    var root = new Ice.Customization.Sandbox.FilePath(
        Ice.Customization.Sandbox.ServerFolder.CompanyData,
        "Herbert/" + runId.ToString("N") + "/" + operationId.ToString("N"));
    Func<string, Ice.Customization.Sandbox.FilePath> path =
        leaf => new Ice.Customization.Sandbox.FilePath(root, leaf);
    Func<string, string> safeName = name =>
    {
        if (String.IsNullOrEmpty(name) || name.StartsWith("/") || name.Contains("\\") ||
            name.Split('/').Any(p => p.Length == 0 || p == "." || p == ".." ||
                p.EndsWith(".") || p.EndsWith(" ") ||
                p.Any(c => Char.IsControl(c) || "<>:\"|?*".Contains(c))))
            throw new Ice.BLException("The file name must be a portable relative path.");
        foreach (var part in name.Split('/'))
        {
            var stem = part.Split('.')[0].ToUpperInvariant();
            if (new[] { "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$" }.Contains(stem) ||
                (stem.Length == 4 && (stem.StartsWith("COM") || stem.StartsWith("LPT")) &&
                 "123456789".Contains(stem[3])))
                throw new Ice.BLException("The file name uses a reserved device name.");
        }
        return name;
    };
    Func<System.IO.Stream, string> hash = stream =>
    {
        using (var sha = System.Security.Cryptography.SHA256.Create())
            return BitConverter.ToString(sha.ComputeHash(stream)).Replace("-", "").ToLowerInvariant();
    };
    Action<Newtonsoft.Json.Linq.JObject> save = state =>
        Sandbox.IO.File.WriteAllText(path("state.json"), state.ToString(Newtonsoft.Json.Formatting.None));
    Action removeEmptyRunDirectory = () =>
    {
        var runDirectory = new Ice.Customization.Sandbox.FilePath(
            Ice.Customization.Sandbox.ServerFolder.CompanyData, "Herbert/" + runId.ToString("N"));
        if (Sandbox.IO.Directory.Exists(runDirectory) &&
            Sandbox.IO.Directory.GetFiles(runDirectory).Length == 0 &&
            Sandbox.IO.Directory.GetDirectories(runDirectory).Length == 0)
            Sandbox.IO.Directory.Delete(runDirectory, false);
    };
    if (action == "begin")
        Sandbox.IO.Directory.CreateDirectory(root);
    if (!Sandbox.IO.Directory.Exists(root))
    {
        if (action != "inspect" && action != "purge" && action != "close")
            throw new Ice.BLException("The staged operation does not exist.");
        oJson = new Newtonsoft.Json.Linq.JObject { ["exists"] = false }.ToString();
        return;
    }
    // A process can stop after it removes the ownership file during final cleanup.
    // Inspect this empty directory without creating another lock file.
    if (action != "begin" && !Sandbox.IO.File.Exists(path("state.json")) &&
        Sandbox.IO.Directory.GetFiles(root).Length == 0 &&
        Sandbox.IO.Directory.GetDirectories(root).Length == 0)
    {
        if (action == "purge")
        {
            Sandbox.IO.Directory.Delete(root, false);
            removeEmptyRunDirectory();
            oJson = new Newtonsoft.Json.Linq.JObject { ["purged"] = true }.ToString();
            return;
        }
        if (action == "inspect")
        {
            oJson = new Newtonsoft.Json.Linq.JObject { ["exists"] = true, ["phase"] = "empty" }.ToString();
            return;
        }
        if (action == "close")
        {
            oJson = new Newtonsoft.Json.Linq.JObject { ["closed"] = true }.ToString();
            return;
        }
        throw new Ice.BLException("The staging directory has no ownership record.");
    }
    // Company storage is shared. Content access still binds company and user.
    // Published inputs are immutable. Shared readers can inspect and transfer them
    // while the producer owns the writer. Mutations and cleanup remain exclusive.
    var readingPublishedInputs = action == "pollInputs" || action == "readInputs";
    using (var guard = Sandbox.IO.File.Open(path("writer.lock"),
        readingPublishedInputs ? System.IO.FileMode.Open : System.IO.FileMode.OpenOrCreate,
        readingPublishedInputs ? System.IO.FileAccess.Read : System.IO.FileAccess.ReadWrite,
        readingPublishedInputs ? System.IO.FileShare.ReadWrite : System.IO.FileShare.None))
    {
        leaseGuard.Dispose();
        Newtonsoft.Json.Linq.JObject state;
        if (Sandbox.IO.File.Exists(path("state.json")))
        {
            state = Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(path("state.json")));
            if (state.Value<string>("runId") != runId.ToString("D") ||
                state.Value<string>("operationId") != operationId.ToString("D") ||
                state.Value<string>("company") != Session.CompanyID ||
                state.Value<string>("caller") != Session.UserID)
                throw new Ice.BLException("The staged operation belongs to another scope.");
            var recordedInputs = state["inputs"] as Newtonsoft.Json.Linq.JArray;
            if (recordedInputs == null)
                throw new Ice.BLException("The ownership record has no input list.");
            foreach (var entry in recordedInputs)
            {
                var leaf = (string)entry["fileId"];
                Guid recordedId;
                if (leaf == null || !leaf.EndsWith(".input") ||
                    !Guid.TryParseExact(leaf.Substring(0, leaf.Length - 6), "N", out recordedId) ||
                    recordedId == Guid.Empty || leaf != recordedId.ToString("N") + ".input")
                    throw new Ice.BLException("The ownership record contains an invalid file ID.");
                safeName((string)entry["name"]);
            }
        }
        else
        {
            if (action != "begin")
                throw new Ice.BLException("The staged operation has no ownership record.");
            if (Sandbox.IO.Directory.GetDirectories(root).Length != 0 ||
                Sandbox.IO.Directory.GetFiles(root).Any(f => !f.Subpath.EndsWith("/writer.lock") && !f.Subpath.EndsWith("\\writer.lock")))
                throw new Ice.BLException("The staging directory is not empty.");
            state = new Newtonsoft.Json.Linq.JObject
            {
                ["runId"] = runId.ToString("D"),
                ["operationId"] = operationId.ToString("D"),
                ["company"] = Session.CompanyID,
                ["caller"] = Session.UserID,
                ["phase"] = "open",
                ["inputs"] = new Newtonsoft.Json.Linq.JArray()
            };
            save(state);
        }
        // List once for inventory operations. On remote company storage, one
        // existence request per reserved file makes an empty poll take many seconds.
        // A snapshot is sufficient: published files are immutable, and a marker
        // created after this snapshot is returned by the next poll.
        var existingFiles = new HashSet<string>(StringComparer.Ordinal);
        if (action == "begin" || action == "inspect" || action == "pollInputs" ||
            action == "reserve" || action == "close")
            foreach (var file in Sandbox.IO.Directory.GetFiles(root))
                existingFiles.Add(file.Subpath.Replace('\\', '/').Split('/').Last());
        if (action == "begin" || action == "inspect")
        {
            // Report existing reserved files so the host can verify delivery before cleanup.
            var readyInputs = new Newtonsoft.Json.Linq.JArray();
            foreach (var input in (Newtonsoft.Json.Linq.JArray)state["inputs"])
            {
                var leaf = (string)input["fileId"];
                if (!existingFiles.Contains(leaf))
                    continue;
                using (var stream = Sandbox.IO.File.OpenRead(path(leaf)))
                    readyInputs.Add(new Newtonsoft.Json.Linq.JObject
                    {
                        ["fileId"] = leaf,
                        ["name"] = (string)input["name"],
                        ["bytes"] = stream.Length,
                        ["sha256"] = hash(stream)
                    });
            }
            state["readyInputs"] = readyInputs;
            state["exists"] = true;
            state["folder"] = "CompanyData";
            state["relativePath"] = root.Subpath;
            state["lockRelativePath"] = path("writer.lock").Subpath;
            oJson = state.ToString(Newtonsoft.Json.Formatting.None);
            return;
        }
        if (action == "pollInputs")
        {
            if (state.Value<string>("phase") != "open")
                throw new Ice.BLException("The input operation is not open.");
            var completed = new Newtonsoft.Json.Linq.JArray();
            var failed = 0;
            var active = 0;
            foreach (var input in state["inputs"])
            {
                var leaf = (string)input["fileId"];
                if (existingFiles.Contains(leaf + ".ready"))
                {
                    var receipt = Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(path(leaf + ".receipt.json")));
                    if ((string)receipt["fileId"] != leaf || (string)receipt["name"] != (string)input["name"])
                        throw new Ice.BLException("A published receipt does not match its reservation.");
                    // Return generic transfer facts, not plugin-specific receipt fields.
                    completed.Add(new Newtonsoft.Json.Linq.JObject
                    {
                        ["fileId"] = leaf,
                        ["name"] = receipt["name"],
                        ["bytes"] = receipt["bytes"],
                        ["sha256"] = receipt["sha256"]
                    });
                }
                else if (existingFiles.Contains(leaf + ".failed"))
                    failed++;
                else if (existingFiles.Contains(leaf + ".started"))
                    active++;
            }
            oJson = new Newtonsoft.Json.Linq.JObject
            {
                ["files"] = completed,
                ["failed"] = failed,
                ["active"] = active
            }.ToString(Newtonsoft.Json.Formatting.None);
            return;
        }
        if (action == "reserve")
        {
            if (state.Value<string>("phase") != "open")
                throw new Ice.BLException("The operation is not open for input.");
            var slots = request["files"] as Newtonsoft.Json.Linq.JArray;
            if (slots == null)
                throw new Ice.BLException("An explicit file list is required.");
            var inputs = (Newtonsoft.Json.Linq.JArray)state["inputs"];
            var pending = (Newtonsoft.Json.Linq.JArray)inputs.DeepClone();
            var result = new Newtonsoft.Json.Linq.JArray();
            foreach (Newtonsoft.Json.Linq.JObject slot in slots)
            {
                Guid fileId;
                if (!Guid.TryParse(slot.Value<string>("fileId"), out fileId) || fileId == Guid.Empty)
                    throw new Ice.BLException("A nonempty file ID is required.");
                var leaf = fileId.ToString("N") + ".input";
                var name = safeName(slot.Value<string>("name"));
                var existing = pending.FirstOrDefault(f => (string)f["fileId"] == leaf);
                if (existing != null && (string)existing["name"] != name)
                    throw new Ice.BLException("The file ID belongs to another name.");
                if (pending.Any(f => (string)f["fileId"] != leaf &&
                    String.Equals((string)f["name"], name, StringComparison.OrdinalIgnoreCase)))
                    throw new Ice.BLException("Output names collide.");
                if (existing == null)
                {
                    if (existingFiles.Contains(leaf))
                        throw new Ice.BLException("The reserved file already exists without an ownership record.");
                    pending.Add(new Newtonsoft.Json.Linq.JObject { ["fileId"] = leaf, ["name"] = name });
                }
                result.Add(new Newtonsoft.Json.Linq.JObject
                {
                    ["fileId"] = leaf,
                    ["name"] = name,
                    ["folder"] = "CompanyData",
                    ["relativePath"] = path(leaf).Subpath
                });
            }
            // Record ownership before a plugin helper writes the reserved files.
            state["inputs"] = pending;
            save(state);
            oJson = new Newtonsoft.Json.Linq.JObject
            {
                ["files"] = result,
                ["lockRelativePath"] = path("writer.lock").Subpath
            }.ToString();
            return;
        }
        if (action == "put")
        {
            if (state.Value<string>("phase") != "open")
                throw new Ice.BLException("The operation is not open for input.");
            var name = safeName(request.Value<string>("name"));
            var leaf = request.Value<string>("fileId");
            Guid fileId;
            if (!Guid.TryParse(leaf, out fileId) || fileId == Guid.Empty)
                throw new Ice.BLException("A file ID is required.");
            leaf = fileId.ToString("N") + ".input";
            var inputs = (Newtonsoft.Json.Linq.JArray)state["inputs"];
            var existing = inputs.OfType<Newtonsoft.Json.Linq.JObject>().FirstOrDefault(f => f.Value<string>("fileId") == leaf);
            if (existing != null && existing.Value<string>("name") != name)
                throw new Ice.BLException("The file ID is already bound to another name.");
            if (inputs.Any(f => String.Equals((string)f["name"], name, StringComparison.OrdinalIgnoreCase) && (string)f["fileId"] != leaf))
                throw new Ice.BLException("Output names collide.");
            var bytes = Convert.FromBase64String(request.Value<string>("data"));
            var offset = request.Value<long>("offset");
            if (offset < 0)
                throw new Ice.BLException("The input offset cannot be negative.");
            if (existing == null)
            {
                if (offset != 0 || Sandbox.IO.File.Exists(path(leaf)))
                    throw new Ice.BLException("A new input must begin at offset zero.");
                existing = new Newtonsoft.Json.Linq.JObject { ["fileId"] = leaf, ["name"] = name };
                inputs.Add(existing);
                save(state); // Record the exact owned file before creating it.
            }
            using (var output = Sandbox.IO.File.Open(path(leaf), System.IO.FileMode.OpenOrCreate, System.IO.FileAccess.ReadWrite))
            {
                if (output.Length == offset)
                {
                    output.Position = offset;
                    output.Write(bytes, 0, bytes.Length);
                }
                else if (offset <= output.Length && bytes.LongLength <= output.Length - offset)
                {
                    output.Position = offset;
                    var prior = new byte[bytes.Length];
                    var read = 0;
                    while (read < prior.Length)
                    {
                        var n = output.Read(prior, read, prior.Length - read);
                        if (n == 0)
                            break;
                        read += n;
                    }
                    if (read != prior.Length || !prior.SequenceEqual(bytes))
                        throw new Ice.BLException("An input retry differs from the saved bytes.");
                }
                else
                    throw new Ice.BLException("The input offset does not match the staged file.");
                oJson = new Newtonsoft.Json.Linq.JObject { ["fileId"] = leaf, ["length"] = output.Length }.ToString();
            }
            return;
        }
        if (action == "seal")
        {
            var manifest = request["files"] as Newtonsoft.Json.Linq.JArray;
            if (manifest == null)
                throw new Ice.BLException("An explicit file manifest is required.");
            var compression = request.Value<string>("compression");
            if (compression != "store" && compression != "deflate")
                throw new Ice.BLException("Select store or deflate compression.");
            var signature = new Newtonsoft.Json.Linq.JObject { ["files"] = manifest, ["compression"] = compression }.ToString(Newtonsoft.Json.Formatting.None);
            if (state.Value<string>("phase") == "ready")
            {
                if (state.Value<string>("request") != signature)
                    throw new Ice.BLException("This operation already packaged a different request.");
                using (var archive = Sandbox.IO.File.OpenRead(path("archive.zip")))
                    if (archive.Length != state.Value<long>("bytes") || hash(archive) != state.Value<string>("sha256"))
                        throw new Ice.BLException("The ready archive has changed.");
                oJson = state.ToString(Newtonsoft.Json.Formatting.None);
                return;
            }
            if (state.Value<string>("phase") != "open" && state.Value<string>("phase") != "packaging")
                throw new Ice.BLException("This operation cannot package files.");
            if (state.Value<string>("phase") != "open" && state.Value<string>("request") != signature)
                throw new Ice.BLException("The packaging request changed.");
            var names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var leaves = new HashSet<string>(StringComparer.Ordinal);
            foreach (Newtonsoft.Json.Linq.JObject entry in manifest)
            {
                var name = safeName(entry.Value<string>("name"));
                var leaf = entry.Value<string>("fileId");
                var owned = state["inputs"].FirstOrDefault(f => (string)f["fileId"] == leaf && (string)f["name"] == name);
                if (owned == null || !names.Add(name) || !leaves.Add(leaf))
                    throw new Ice.BLException("The manifest contains an unowned or duplicate input.");
                using (var input = Sandbox.IO.File.OpenRead(path(leaf)))
                    if (input.Length != entry.Value<long>("bytes") || hash(input) != entry.Value<string>("sha256"))
                        throw new Ice.BLException("A staged input does not match its manifest.");
            }
            foreach (var name in names)
            {
                var parts = name.Split('/');
                var prefix = parts[0];
                for (var i = 1; i < parts.Length; i++)
                {
                    if (names.Contains(prefix))
                        throw new Ice.BLException("A file conflicts with a directory name.");
                    prefix += "/" + parts[i];
                }
            }
            state["phase"] = "packaging";
            state["request"] = signature;
            save(state);
            using (var output = Sandbox.IO.File.Open(path("archive.zip"), System.IO.FileMode.Create, System.IO.FileAccess.Write))
            using (var zip = new System.IO.Compression.ZipArchive(output, System.IO.Compression.ZipArchiveMode.Create))
                foreach (Newtonsoft.Json.Linq.JObject entry in manifest)
                {
                    var member = zip.CreateEntry(entry.Value<string>("name"), compression == "store" ?
                        System.IO.Compression.CompressionLevel.NoCompression : System.IO.Compression.CompressionLevel.Optimal);
                    using (var input = Sandbox.IO.File.OpenRead(path(entry.Value<string>("fileId"))))
                    using (var destination = member.Open())
                        input.CopyTo(destination);
                }
            using (var archive = Sandbox.IO.File.OpenRead(path("archive.zip")))
            {
                state["bytes"] = archive.Length;
                state["sha256"] = hash(archive);
            }
            state["files"] = manifest;
            state["phase"] = "ready";
            save(state);
            oJson = state.ToString(Newtonsoft.Json.Formatting.None);
            return;
        }
        if (action == "readInputs")
        {
            // Transfer native files in one response. The caller chooses the group.
            // Large files continue from their verified offsets without an outer ZIP.
            var reads = request["files"] as Newtonsoft.Json.Linq.JArray;
            if (state.Value<string>("phase") != "open" || reads == null || reads.Count == 0)
                throw new Ice.BLException("Supply reserved input files from an open operation.");
            var seenReads = new HashSet<string>(StringComparer.Ordinal);
            foreach (var readRequest in reads)
            {
                var leaf = (string)readRequest["fileId"];
                if (!seenReads.Add(leaf) || !state["inputs"].Any(input => (string)input["fileId"] == leaf) ||
                    (long)readRequest["offset"] < 0 || (int)readRequest["count"] <= 0)
                    throw new Ice.BLException("A batch input, offset, or buffer length is invalid.");
            }
            var results = new Newtonsoft.Json.Linq.JArray();
            foreach (var readRequest in reads)
            {
                var leaf = (string)readRequest["fileId"];
                var offset = (long)readRequest["offset"];
                var count = (int)readRequest["count"];
                using (var input = Sandbox.IO.File.OpenRead(path(leaf)))
                {
                    if (offset > input.Length)
                        throw new Ice.BLException("The offset exceeds the input length.");
                    var sha256 = hash(input);
                    input.Position = offset;
                    var bytes = new byte[(int)Math.Min((long)count, input.Length - offset)];
                    var read = 0;
                    while (read < bytes.Length)
                    {
                        var n = input.Read(bytes, read, bytes.Length - read);
                        if (n == 0)
                            throw new Ice.BLException("The input ended unexpectedly.");
                        read += n;
                    }
                    results.Add(new Newtonsoft.Json.Linq.JObject
                    {
                        ["fileId"] = leaf,
                        ["offset"] = offset,
                        ["total"] = input.Length,
                        ["data"] = Convert.ToBase64String(bytes),
                        ["sha256"] = sha256
                    });
                }
            }
            oJson = new Newtonsoft.Json.Linq.JObject { ["files"] = results }
                .ToString(Newtonsoft.Json.Formatting.None);
            return;
        }
        if (action == "readInput")
        {
            // Already-compressed native files do not need an outer archive.
            // Read only a reserved input and report its current content hash.
            var leaf = request.Value<string>("fileId");
            if (state.Value<string>("phase") != "open" ||
                !state["inputs"].Any(input => (string)input["fileId"] == leaf))
                throw new Ice.BLException("The requested input is not open and reserved.");
            var offset = request.Value<long>("offset");
            var count = request.Value<int>("count");
            if (offset < 0 || count <= 0)
                throw new Ice.BLException("A nonnegative offset and positive buffer length are required.");
            using (var input = Sandbox.IO.File.OpenRead(path(leaf)))
            {
                if (offset > input.Length)
                    throw new Ice.BLException("The offset exceeds the input length.");
                var sha256 = hash(input);
                input.Position = offset;
                var bytes = new byte[(int)Math.Min((long)count, input.Length - offset)];
                var read = 0;
                while (read < bytes.Length)
                {
                    var n = input.Read(bytes, read, bytes.Length - read);
                    if (n == 0)
                        throw new Ice.BLException("The input ended unexpectedly.");
                    read += n;
                }
                oJson = new Newtonsoft.Json.Linq.JObject
                {
                    ["fileId"] = leaf,
                    ["offset"] = offset,
                    ["total"] = input.Length,
                    ["data"] = Convert.ToBase64String(bytes),
                    ["sha256"] = sha256
                }.ToString();
            }
            return;
        }
        if (action == "read")
        {
            if (state.Value<string>("phase") != "ready")
                throw new Ice.BLException("The archive is not ready.");
            var offset = request.Value<long>("offset");
            var count = request.Value<int>("count"); // Stream buffer/API length is Int32; no smaller policy limit.
            if (offset < 0 || count <= 0)
                throw new Ice.BLException("A nonnegative offset and positive buffer length are required.");
            using (var input = Sandbox.IO.File.OpenRead(path("archive.zip")))
            {
                if (offset > input.Length)
                    throw new Ice.BLException("The offset exceeds the archive length.");
                input.Position = offset;
                var bytes = new byte[(int)Math.Min((long)count, input.Length - offset)];
                var read = 0;
                while (read < bytes.Length)
                {
                    var n = input.Read(bytes, read, bytes.Length - read);
                    if (n == 0)
                        throw new Ice.BLException("The archive ended unexpectedly.");
                    read += n;
                }
                oJson = new Newtonsoft.Json.Linq.JObject
                {
                    ["offset"] = offset,
                    ["total"] = input.Length,
                    ["data"] = Convert.ToBase64String(bytes),
                    ["sha256"] = state["sha256"]
                }.ToString();
            }
            return;
        }
        if (action == "close")
        {
            // The host must verify local files before requesting successful-result cleanup.
            if (state.Value<string>("phase") == "ready" && request.Value<string>("sha256") != state.Value<string>("sha256"))
                throw new Ice.BLException("The verified archive hash is required for cleanup.");
            foreach (var entry in state["inputs"])
            {
                var leaf = (string)entry["fileId"];
                if (existingFiles.Contains(leaf))
                    Sandbox.IO.File.Delete(path(leaf));
                // Delete only exact reserved sidecars from the snapshot. Preserve
                // unknown files so the final purge check can report them.
                foreach (var suffix in new[] { ".receipt.json", ".ready", ".failed", ".started" })
                    if (existingFiles.Contains(leaf + suffix))
                        Sandbox.IO.File.Delete(path(leaf + suffix));
            }
            if (Sandbox.IO.File.Exists(path("archive.zip")))
                Sandbox.IO.File.Delete(path("archive.zip"));
            if (Sandbox.IO.File.Exists(path("producer.started")))
                Sandbox.IO.File.Delete(path("producer.started"));
            state["phase"] = "closed";
            save(state);
            oJson = new Newtonsoft.Json.Linq.JObject { ["closed"] = true }.ToString();
            return;
        }
        if (action != "purge")
            throw new Ice.BLException("Unknown host utility operation.");
        if (state.Value<string>("phase") != "closed")
            throw new Ice.BLException("Close this operation before removing its ownership record.");
        if (Sandbox.IO.Directory.GetDirectories(root).Length != 0 ||
            Sandbox.IO.Directory.GetFiles(root).Any(f =>
            {
                var leaf = f.Subpath.Replace('\\', '/').Split('/').Last();
                return leaf != "writer.lock" && leaf != "state.json";
            }))
            throw new Ice.BLException("Unrecognized staged files were preserved.");
    }
    // Purge is a separate, host-coordinated final step after all users release this
    // operation. Retrying it sees exists:false; it never restarts collection.
    Sandbox.IO.File.Delete(path("writer.lock"));
    Sandbox.IO.File.Delete(path("state.json"));
    Sandbox.IO.Directory.Delete(root, false);
    removeEmptyRunDirectory();
    oJson = new Newtonsoft.Json.Linq.JObject { ["purged"] = true }.ToString();

}
finally { leaseGuard.Dispose(); }
