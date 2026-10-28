// Offline compatibility shim. This executes the actual Function body; it does
// not claim Epicor compilation, permissions or Sandbox implementation proof.
using System;
using System.IO;
using System.Linq;
using System.Collections.Generic;
using Newtonsoft.Json.Linq;
namespace Ice
{
    public class BLException : Exception
    {
        public BLException(string text) : base(text) { }
    }
}
namespace Ice.Customization.Sandbox
{
    public enum ServerFolder
    {
        UserData, CompanyData
    }
    public class FilePath
    {
        public string Subpath;
        public FilePath(ServerFolder folder, string path)
        {
            Subpath = path;
        }
        public FilePath(FilePath root, string leaf)
        {
            Subpath = root.Subpath + "/" + leaf;
        }
    }
}
public static class Sandbox
{
    public static string Root = "";
    public static class IO
    {
        private static string Path(Ice.Customization.Sandbox.FilePath p) => System.IO.Path.Combine(Root, p.Subpath);
        public static class File
        {
            public static FileStream Open(Ice.Customization.Sandbox.FilePath p, FileMode m, FileAccess a) => System.IO.File.Open(Path(p), m, a);
            public static FileStream Open(Ice.Customization.Sandbox.FilePath p, FileMode m, FileAccess a, FileShare s) => System.IO.File.Open(Path(p), m, a, s);
            public static FileStream OpenRead(Ice.Customization.Sandbox.FilePath p) => System.IO.File.OpenRead(Path(p));
            public static bool Exists(Ice.Customization.Sandbox.FilePath p) => System.IO.File.Exists(Path(p));
            public static void WriteAllText(Ice.Customization.Sandbox.FilePath p, string s) => System.IO.File.WriteAllText(Path(p), s);
            public static string ReadAllText(Ice.Customization.Sandbox.FilePath p) => System.IO.File.ReadAllText(Path(p));
            public static void Delete(Ice.Customization.Sandbox.FilePath p) => System.IO.File.Delete(Path(p));
            public static void Move(Ice.Customization.Sandbox.FilePath source, Ice.Customization.Sandbox.FilePath target, bool overwrite) => System.IO.File.Move(Path(source), Path(target), overwrite);
        }
        public static class Directory
        {
            public static void CreateDirectory(Ice.Customization.Sandbox.FilePath p) => System.IO.Directory.CreateDirectory(Path(p));
            public static bool Exists(Ice.Customization.Sandbox.FilePath p) => System.IO.Directory.Exists(Path(p));
            public static void Delete(Ice.Customization.Sandbox.FilePath p, bool recursive) => System.IO.Directory.Delete(Path(p), recursive);
            public static Ice.Customization.Sandbox.FilePath[] GetFiles(Ice.Customization.Sandbox.FilePath p) => System.IO.Directory.GetFiles(Path(p)).Select(f => new Ice.Customization.Sandbox.FilePath(p, System.IO.Path.GetFileName(f))).ToArray();
            public static Ice.Customization.Sandbox.FilePath[] GetDirectories(Ice.Customization.Sandbox.FilePath p) => System.IO.Directory.GetDirectories(Path(p)).Select(f => new Ice.Customization.Sandbox.FilePath(p, System.IO.Path.GetFileName(f))).ToArray();
        }
    }
}
public class SessionData
{
    public string CompanyID = "TESTCO"; public string UserID = "test";
}
public partial class Context
{
    public SessionData Session = new SessionData();
    private string iRequest = "", oJson = "";
    public JObject Execute(JObject request)
    {
        iRequest = request.ToString();
        oJson = "";
        Run();
        return JObject.Parse(oJson);
    }
}
public static class Program
{
    private static int assertions;
    static void Check(bool condition, string name)
    {
        assertions++;
        if (!condition)
            throw new Exception(name);
    }
    static void Refuse(Action action, string name)
    {
        bool failed = false;
        try
        {
            action();
        }
        catch (Exception) { failed = true; }
        Check(failed, name);
    }
    static string Hash(byte[] data) => Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(data)).ToLowerInvariant();
    public static void Main()
    {
        Sandbox.Root = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "herbert-host-test-" + Guid.NewGuid().ToString("N"));
        System.IO.Directory.CreateDirectory(Sandbox.Root);
        try
        {
            var c = new Context();
            LeaseChecks.Run();
            var run = Guid.NewGuid().ToString();
            var op = Guid.NewGuid().ToString();
            var generation = Guid.NewGuid();
            c.Lease(new JObject
            {
                ["action"] = "acquire",
                ["operationId"] = run,
                ["resources"] = new JArray(new JObject
                {
                    ["libraryId"] = "H" + generation.ToString("N").Substring(0, 29),
                    ["globalId"] = generation.ToString(),
                    ["role"] = "host"
                })
            });
            Func<string, JObject, JObject> call = (action, fields) =>
            {
                var request = fields ?? new JObject();
                request["action"] = action;
                request["runId"] = run;
                request["operationId"] = op;
                return c.Execute(request);
            };
            Check((bool)call("inspect", null)["exists"] == false, "missing inspect");
            Refuse(() => call("read", new JObject { ["offset"] = 0, ["count"] = 1 }), "absent read is not success");
            var begin = call("begin", null);
            Check((string)begin["phase"] == "open", "begin");
            Check((string)call("begin", null)["phase"] == "open", "begin idempotent");
            c.Session.CompanyID = "OTHER";
            Refuse(() => call("inspect", null), "company ownership");
            c.Session.CompanyID = "TESTCO";
            c.Session.UserID = "OTHER";
            Refuse(() => call("inspect", null), "caller ownership");
            c.Session.UserID = "test";
            foreach (var name in new[] { "../escape", "/absolute", "a\\b", "a/./b", "AUX.txt", "a//b", "a. ", "a:b" })
            {
                Refuse(() => call("put", new JObject { ["fileId"] = Guid.NewGuid().ToString(), ["name"] = name, ["offset"] = 0, ["data"] = "" }), "unsafe " + name);
            }
            var payload = System.Text.Encoding.UTF8.GetBytes("Hello π — actual source\n");
            var fileId = Guid.NewGuid().ToString();
            var put = new JObject { ["fileId"] = fileId, ["name"] = "nested/π.json", ["offset"] = 0, ["data"] = Convert.ToBase64String(payload) };
            var staged = call("put", (JObject)put.DeepClone());
            Check((long)staged["length"] == payload.Length, "put");
            Check((long)call("put", (JObject)put.DeepClone())["length"] == payload.Length, "retry exact bytes");
            var changed = (JObject)put.DeepClone();
            changed["data"] = Convert.ToBase64String(new byte[] { 99 });
            Refuse(() => call("put", changed), "changed retry");
            var gap = (JObject)put.DeepClone();
            gap["offset"] = payload.Length + 1;
            Refuse(() => call("put", gap), "offset gap");
            var emptyId = Guid.NewGuid().ToString();
            var empty = call("put", new JObject { ["fileId"] = emptyId, ["name"] = "empty.json", ["offset"] = 0, ["data"] = "" });
            var files = new JArray(
                new JObject { ["fileId"] = staged["fileId"], ["name"] = "nested/π.json", ["bytes"] = payload.Length, ["sha256"] = Hash(payload) },
                new JObject { ["fileId"] = empty["fileId"], ["name"] = "empty.json", ["bytes"] = 0, ["sha256"] = Hash(Array.Empty<byte>()) });
            var badFiles = (JArray)files.DeepClone();
            badFiles[0]["sha256"] = new string('0', 64);
            Refuse(() => call("seal", new JObject { ["files"] = badFiles, ["compression"] = "deflate" }), "wrong input hash");
            var seal = new JObject { ["files"] = files, ["compression"] = "deflate" };
            var ready = call("seal", (JObject)seal.DeepClone());
            Check((string)ready["phase"] == "ready", "seal");
            Check((string)call("seal", (JObject)seal.DeepClone())["sha256"] == (string)ready["sha256"], "seal idempotent");
            Refuse(() => call("put", put), "closed input");
            Refuse(() => call("seal", new JObject { ["files"] = files, ["compression"] = "store" }), "changed seal");
            var received = new MemoryStream();
            long offset = 0, total = (long)ready["bytes"];
            while (offset < total)
            {
                var part = call("read", new JObject { ["offset"] = offset, ["count"] = 7 });
                var bytes = Convert.FromBase64String((string)part["data"]);
                Check((long)part["offset"] == offset && (long)part["total"] == total, "read metadata");
                received.Write(bytes);
                offset += bytes.Length;
            }
            Check(Hash(received.ToArray()) == (string)ready["sha256"], "archive hash");
            received.Position = 0;
            using (var zip = new System.IO.Compression.ZipArchive(received, System.IO.Compression.ZipArchiveMode.Read, true))
            {
                Check(zip.Entries.Count == 2, "member count");
                using (var s = zip.GetEntry("nested/π.json").Open())
                using (var output = new MemoryStream())
                {
                    s.CopyTo(output);
                    Check(output.ToArray().SequenceEqual(payload), "member bytes");
                }
                Check(zip.GetEntry("empty.json").Length == 0, "empty member");
            }
            Refuse(() => call("close", new JObject { ["sha256"] = "bad" }), "cleanup needs correct hash");
            var directory = System.IO.Path.Combine(Sandbox.Root, (string)begin["relativePath"]);
            System.IO.File.WriteAllText(System.IO.Path.Combine(directory, "unknown.txt"), "retain me");
            Check((bool)call("close", new JObject { ["sha256"] = ready["sha256"] })["closed"], "close");
            Refuse(() => call("seal", seal), "cannot reopen closed operation");
            Refuse(() => call("purge", null), "unknown files prevent purge");
            Check(System.IO.File.ReadAllText(System.IO.Path.Combine(directory, "unknown.txt")) == "retain me", "unknown content preserved");
            System.IO.File.Delete(System.IO.Path.Combine(directory, "unknown.txt"));
            Check((bool)call("purge", null)["purged"], "purge");
            Check((bool)call("purge", null)["exists"] == false, "purge retry");
            Check(!System.IO.Directory.Exists(directory), "no operation leftovers");
            // Portable prefix collisions must fail before creating an archive.
            op = Guid.NewGuid().ToString();
            call("begin", null);
            var first = call("put", new JObject { ["fileId"] = Guid.NewGuid().ToString(), ["name"] = "a", ["offset"] = 0, ["data"] = "" });
            var second = call("put", new JObject { ["fileId"] = Guid.NewGuid().ToString(), ["name"] = "A/b", ["offset"] = 0, ["data"] = "" });
            var colliding = new JArray(new JObject { ["fileId"] = first["fileId"], ["name"] = "a", ["bytes"] = 0, ["sha256"] = Hash(Array.Empty<byte>()) },
                new JObject { ["fileId"] = second["fileId"], ["name"] = "A/b", ["bytes"] = 0, ["sha256"] = Hash(Array.Empty<byte>()) });
            Refuse(() => call("seal", new JObject { ["files"] = colliding, ["compression"] = "store" }), "prefix collision");
            call("close", null);
            call("purge", null);
            // A plugin helper writes reserved paths without a host upload.
            op = Guid.NewGuid().ToString();
            begin = call("begin", null);
            var slotId = Guid.NewGuid().ToString();
            var slots = new JObject { ["files"] = new JArray(new JObject { ["fileId"] = slotId, ["name"] = "server/export.json" }) };
            var reserved = call("reserve", (JObject)slots.DeepClone());
            var reservedPath = System.IO.Path.Combine(Sandbox.Root, (string)reserved["files"][0]["relativePath"]);
            Check(!System.IO.File.Exists(reservedPath), "reservation does not create an empty payload");
            var lockPath = System.IO.Path.Combine(Sandbox.Root, (string)reserved["lockRelativePath"]);
            using (var writer = System.IO.File.Open(lockPath, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
            {
                System.IO.File.WriteAllBytes(reservedPath, payload);
                Refuse(() => call("close", null), "cleanup cannot run during a plugin write");
            }
            call("reserve", (JObject)slots.DeepClone());
            Check(System.IO.File.ReadAllBytes(reservedPath).SequenceEqual(payload), "reserve retry preserves completed server work");
            var inputRead = new JObject { ["fileId"] = reserved["files"][0]["fileId"], ["offset"] = 0, ["count"] = payload.Length };
            var inputReply = call("readInput", (JObject)inputRead.DeepClone());
            Check(Convert.FromBase64String((string)inputReply["data"]).SequenceEqual(payload), "reserved input transfers without an outer ZIP");
            Check((string)inputReply["sha256"] == Hash(payload), "direct input reports the content hash");
            Check(!System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetDirectoryName(reservedPath), "archive.zip")), "direct input does not create an archive");
            var invalidInput = (JObject)inputRead.DeepClone();
            invalidInput["fileId"] = "../state.json";
            Refuse(() => call("readInput", invalidInput), "direct input rejects unreserved paths");
            invalidInput = (JObject)inputRead.DeepClone();
            invalidInput["count"] = 0;
            Refuse(() => call("readInput", invalidInput), "direct input requires a positive buffer");
            files = new JArray(new JObject { ["fileId"] = reserved["files"][0]["fileId"], ["name"] = "server/export.json", ["bytes"] = payload.Length, ["sha256"] = Hash(payload) });
            seal = new JObject { ["files"] = files, ["compression"] = "store" };
            ready = call("seal", (JObject)seal.DeepClone());
            directory = System.IO.Path.Combine(Sandbox.Root, (string)begin["relativePath"]);
            var archivePath = System.IO.Path.Combine(directory, "archive.zip");
            System.IO.File.WriteAllText(archivePath, "corrupt");
            Refuse(() => call("seal", (JObject)seal.DeepClone()), "ready archive corruption is not successful retry");
            // Reproduce a process exit after the packaging intent but before completion.
            var statePath = System.IO.Path.Combine(directory, "state.json");
            var saved = JObject.Parse(System.IO.File.ReadAllText(statePath));
            saved["phase"] = "packaging";
            System.IO.File.WriteAllText(statePath, saved.ToString());
            ready = call("seal", (JObject)seal.DeepClone());
            Check((string)ready["phase"] == "ready", "interrupted packaging resumes from staged files");
            Check(System.IO.File.ReadAllBytes(reservedPath).SequenceEqual(payload), "resume preserves plugin output");
            saved = JObject.Parse(System.IO.File.ReadAllText(statePath));
            var tampered = (JObject)saved.DeepClone();
            tampered["inputs"][0]["fileId"] = "../outside.input";
            System.IO.File.WriteAllText(statePath, tampered.ToString());
            Refuse(() => call("close", new JObject { ["sha256"] = ready["sha256"] }), "corrupt ownership cannot select a deletion path");
            Check(System.IO.File.Exists(reservedPath), "invalid ownership stops before deletion");
            System.IO.File.WriteAllText(statePath, saved.ToString());
            call("close", new JObject { ["sha256"] = ready["sha256"] });
            // Reproduce interruption between final file removal and directory removal.
            System.IO.File.Delete(System.IO.Path.Combine(directory, "writer.lock"));
            System.IO.File.Delete(statePath);
            Check((string)call("inspect", null)["phase"] == "empty", "inspect does not recreate a lock during final cleanup");
            call("purge", null);
            Check(!System.IO.Directory.Exists(directory), "final cleanup resumes after ownership-file removal");
            Console.WriteLine("PASS: " + assertions + " assertions; actual helper source executed with local Sandbox shim.");
        }
        finally
        {
            // Exact test-created directory, never a caller-selected cleanup path.
            System.IO.Directory.Delete(Sandbox.Root, true);
        }
    }
}
