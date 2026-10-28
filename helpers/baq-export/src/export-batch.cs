// Export selected BAQs into files reserved by the shared host utility.
// One invocation accepts the complete selection. The caller selects parallelism.
oSuccess = false;
oMessage = "";
oJson = "";
var request = Newtonsoft.Json.Linq.JObject.Parse(iRequest);
var concurrency = request.Value<int?>("serverConcurrency");
if (!concurrency.HasValue || concurrency.Value <= 0)
    throw new Ice.BLException("Supply a positive server concurrency value.");
Guid runId, operationId;
if (!Guid.TryParse(request.Value<string>("runId"), out runId) || runId == Guid.Empty ||
    !Guid.TryParse(request.Value<string>("operationId"), out operationId) || operationId == Guid.Empty)
    throw new Ice.BLException("A run ID and operation ID are required.");
var items = request["items"] as Newtonsoft.Json.Linq.JArray;
if (items == null || items.Count == 0)
    throw new Ice.BLException("Supply the selected BAQs for this batch.");
var root = new Ice.Customization.Sandbox.FilePath(
    Ice.Customization.Sandbox.ServerFolder.CompanyData,
    "Herbert/" + runId.ToString("N") + "/" + operationId.ToString("N"));
Func<string, Ice.Customization.Sandbox.FilePath> path =
    leaf => new Ice.Customization.Sandbox.FilePath(root, leaf);
Func<System.IO.Stream, string> digest = stream =>
{
    using (var sha = System.Security.Cryptography.SHA256.Create())
        return BitConverter.ToString(sha.ComputeHash(stream)).Replace("-", "").ToLowerInvariant();
};
// Open an existing lock. A collector must never create an unowned operation.
using (var guard = Sandbox.IO.File.Open(path("writer.lock"), System.IO.FileMode.Open,
    System.IO.FileAccess.ReadWrite, System.IO.FileShare.Read))
{
    var state = Newtonsoft.Json.Linq.JObject.Parse(Sandbox.IO.File.ReadAllText(path("state.json")));
    if (state.Value<string>("runId") != runId.ToString("D") ||
        state.Value<string>("operationId") != operationId.ToString("D") ||
        state.Value<string>("company") != Session.CompanyID ||
        state.Value<string>("caller") != Session.UserID || state.Value<string>("phase") != "open")
        throw new Ice.BLException("The operation is not open or belongs to another scope.");
    var inputs = state["inputs"] as Newtonsoft.Json.Linq.JArray;
    if (inputs == null)
        throw new Ice.BLException("The operation has no reserved inputs.");
    var seen = new HashSet<string>(StringComparer.Ordinal);
    // Validate the complete request before exporting or writing any file.
    foreach (var item in items)
    {
        var leaf = (string)item["fileId"];
        Guid fileId;
        if (leaf == null || !leaf.EndsWith(".input") ||
            !Guid.TryParseExact(leaf.Substring(0, leaf.Length - 6), "N", out fileId) ||
            fileId == Guid.Empty || leaf != fileId.ToString("N") + ".input" || !seen.Add(leaf))
            throw new Ice.BLException("A reserved file ID is invalid or repeated.");
        if (String.IsNullOrEmpty((string)item["queryId"]) ||
            ((string)item["company"] != "" && (string)item["company"] != Session.CompanyID))
            throw new Ice.BLException("A query ID or company is invalid.");
        if (!inputs.Any(input => (string)input["fileId"] == leaf &&
            (string)input["name"] == (string)item["name"]))
            throw new Ice.BLException("A batch file does not match its reservation.");
    }
    // The same operation must never submit its selection twice, including after
    // a lost HTTP response. Recovery inspects receipts; it does not replay exports.
    if (Sandbox.IO.File.Exists(path("producer.started")))
        throw new Ice.BLException("This operation already submitted its exports. Reconcile its published files before starting a new operation.");
    using (var submitted = Sandbox.IO.File.Open(path("producer.started"), System.IO.FileMode.CreateNew,
        System.IO.FileAccess.Write, System.IO.FileShare.None)) { }
    var receipts = state["baqExports"] as Newtonsoft.Json.Linq.JObject;
    if (receipts == null)
    {
        receipts = new Newtonsoft.Json.Linq.JObject();
        state["baqExports"] = receipts;
    }
    // Close the data and receipt before publishing the marker. Readers only
    // consume marked files. The operation state stays immutable during export.
    Action<string, string, Newtonsoft.Json.Linq.JObject> publish = (leaf, marker, receipt) =>
    {
        var receiptPath = path(leaf + ".receipt.json");
        using (var output = Sandbox.IO.File.Open(receiptPath, System.IO.FileMode.CreateNew,
            System.IO.FileAccess.Write, System.IO.FileShare.None))
        using (var writer = new System.IO.StreamWriter(output))
            writer.Write(receipt.ToString(Newtonsoft.Json.Formatting.None));
        using (var ready = Sandbox.IO.File.Open(path(leaf + marker), System.IO.FileMode.CreateNew,
            System.IO.FileAccess.Write, System.IO.FileShare.None)) { }
    };
    var files = new Newtonsoft.Json.Linq.JArray();
    var errors = new Newtonsoft.Json.Linq.JArray();
    var receiptLock = new object();
    var activeExports = 0;
    var peakExports = 0;
    // Read authentication only from the current authenticated request. Keep
    // it in memory and bind every child request to this same server and company.
    // RequestUrl can contain the proxy's internal path, such as /server/.
    // AppServerURL restores the public application base through Epicor's
    // original-URI handling. Derive it here; never accept a URL from the caller.
    Uri appServerUrl;
    if (!Uri.TryCreate(Session.AppServerURL.TrimEnd('/') + "/", UriKind.Absolute, out appServerUrl) ||
        appServerUrl.Scheme != "https" || !String.IsNullOrEmpty(appServerUrl.UserInfo) ||
        !String.IsNullOrEmpty(appServerUrl.Query) || !String.IsNullOrEmpty(appServerUrl.Fragment))
        throw new Ice.BLException("The session requires an HTTPS application URL without credentials, query, or fragment.");
    var exportUrl = new Uri(appServerUrl, "api/v2/odata/" +
        Uri.EscapeDataString(Session.CompanyID) + "/Ice.BO.BAQDesignerSvc/ExportBaq");
    // Current is the public accessor in the verified runtime. Its suggested
    // replacement, GetCurrent(), is internal and cannot be called by a Function.
#pragma warning disable CS0618
    var requestHeaders = Epicor.Hosting.Operation.Current.HttpHeaders;
#pragma warning restore CS0618
    Func<string, string> header = name => requestHeaders
        .Where(pair => String.Equals(pair.Key, name, StringComparison.OrdinalIgnoreCase))
        .Select(pair => pair.Value).FirstOrDefault();
    var authorization = header("Authorization");
    var apiKey = header("X-API-Key");
    var sessionInfo = header("SessionInfo");
    var companySettings = new Newtonsoft.Json.Linq.JObject { ["Company"] = Session.CompanyID }
        .ToString(Newtonsoft.Json.Formatting.None);
    if (String.IsNullOrWhiteSpace(authorization))
        throw new Ice.BLException("The current request has no reusable authorization header.");
    // Redirects must not forward authentication to another endpoint.
    using (var handler = new System.Net.Http.HttpClientHandler { AllowAutoRedirect = false })
    using (var client = new System.Net.Http.HttpClient(handler))
    {
        client.Timeout = System.Threading.Timeout.InfiniteTimeSpan;
        System.Threading.Tasks.Parallel.ForEach(items.ToArray(),
            new System.Threading.Tasks.ParallelOptions { MaxDegreeOfParallelism = concurrency.Value }, item =>
        {
            // Each REST request creates an independent native BPM operation.
            var leaf = (string)item["fileId"];
            var queryId = (string)item["queryId"];
            var company = (string)item["company"];
            Newtonsoft.Json.Linq.JObject prior;
            lock (receiptLock)
                prior = receipts[leaf] as Newtonsoft.Json.Linq.JObject;
            if (prior != null)
            {
                if (prior.Value<string>("queryId") != queryId || prior.Value<string>("company") != company)
                    throw new Ice.BLException("The reserved file belongs to another query.");
                using (var saved = Sandbox.IO.File.OpenRead(path(leaf)))
                    if (saved.Length != prior.Value<long>("bytes") || digest(saved) != prior.Value<string>("sha256"))
                        throw new Ice.BLException("A completed batch file has changed.");
                lock (receiptLock)
                    files.Add(prior.DeepClone());
                return;
            }
            // An interrupted write has no receipt. Preserve it for reconciliation.
            if (Sandbox.IO.File.Exists(path(leaf)))
                throw new Ice.BLException("A batch file exists without a completion receipt.");
            // This marker identifies an executing worker, including validation
            // and file publication. It remains until exact operation cleanup.
            using (var started = Sandbox.IO.File.Open(path(leaf + ".started"), System.IO.FileMode.CreateNew,
                System.IO.FileAccess.Write, System.IO.FileShare.None)) { }
            byte[] bytes;
            // Report fixed diagnostic fields. Never return headers, response bodies,
            // exception messages, or request URLs that can contain private values.
            var failureStage = "request";
            int? httpStatus = null;
            try
            {
                lock (receiptLock)
                {
                    activeExports++;
                    peakExports = Math.Max(peakExports, activeExports);
                }
                try
                {
                    using (var message = new System.Net.Http.HttpRequestMessage(System.Net.Http.HttpMethod.Post, exportUrl))
                    {
                        message.Headers.TryAddWithoutValidation("Authorization", authorization);
                        if (!String.IsNullOrEmpty(apiKey)) message.Headers.TryAddWithoutValidation("X-API-Key", apiKey);
                        if (!String.IsNullOrEmpty(sessionInfo)) message.Headers.TryAddWithoutValidation("SessionInfo", sessionInfo);
                        message.Headers.TryAddWithoutValidation("callSettings", companySettings);
                        message.Content = new System.Net.Http.StringContent(
                            new Newtonsoft.Json.Linq.JObject { ["queryId"] = queryId,
                                ["options"] = new Newtonsoft.Json.Linq.JObject() }
                                .ToString(Newtonsoft.Json.Formatting.None), System.Text.Encoding.UTF8, "application/json");
                        using (var response = client.SendAsync(message).GetAwaiter().GetResult())
                        {
                            httpStatus = (int)response.StatusCode;
                            failureStage = "httpStatus";
                            response.EnsureSuccessStatusCode();
                            failureStage = "responseJson";
                            var payload = Newtonsoft.Json.Linq.JObject.Parse(response.Content.ReadAsStringAsync().GetAwaiter().GetResult());
                            var result = payload["returnObj"];
                            failureStage = "exportBytes";
                            bytes = result is Newtonsoft.Json.Linq.JArray ? result.ToObject<byte[]>() :
                                Convert.FromBase64String((string)result);
                        }
                    }
                }
                finally
                {
                    lock (receiptLock)
                        activeExports--;
                }
                failureStage = "archive";
                using (var memory = new System.IO.MemoryStream(bytes, false))
                using (var archive = new System.IO.Compression.ZipArchive(memory, System.IO.Compression.ZipArchiveMode.Read))
                {
                    var entry = archive.GetEntry("DynamicQuery");
                    if (entry == null)
                        throw new Ice.BLException("The native export has no query definition.");
                    failureStage = "queryXml";
                    using (var stream = entry.Open())
                    using (var reader = System.Xml.XmlReader.Create(stream, new System.Xml.XmlReaderSettings
                        { DtdProcessing = System.Xml.DtdProcessing.Prohibit, XmlResolver = null }))
                    {
                        var document = System.Xml.Linq.XDocument.Load(reader);
                        failureStage = "queryIdentity";
                        var headers = document.Root.Elements().Where(node => node.Name.LocalName == "DynamicQueryDesigner").ToArray();
                        if (headers.Length != 1)
                            throw new Ice.BLException("The native export does not identify one query.");
                        Func<string, string> field = name => headers[0].Elements()
                            .Where(node => node.Name.LocalName == name).Select(node => node.Value).SingleOrDefault();
                        if (!String.Equals(field("QueryID"), queryId, StringComparison.OrdinalIgnoreCase) ||
                            field("Company") != company || (field("Updatable") != "false" && field("Updatable") != "0") ||
                            field("SystemFlag") == "true" || field("SystemFlag") == "1")
                            throw new Ice.BLException("The export is not the selected custom non-updatable BAQ.");
                    }
                }
            }
            catch (Exception error)
            {
                // Classify the cause without copying native exception text.
                var failureKind = "other";
                for (var cause = error; cause != null; cause = cause.InnerException)
                {
                    // Read the type name to avoid additional platform assembly references.
                    var causeType = cause.GetType().FullName;
                    if (causeType == "System.Security.Authentication.AuthenticationException") { failureKind = "tls"; break; }
                    if (causeType == "System.Net.Sockets.SocketException")
                    {
                        failureKind = "socket";
                        break;
                    }
                    if (cause is System.Net.Http.HttpRequestException) failureKind = "http";
                }
                var failure = new Newtonsoft.Json.Linq.JObject { ["queryId"] = queryId,
                    ["company"] = company, ["fileId"] = leaf, ["code"] = "BAQ_EXPORT_FAILED",
                    ["stage"] = failureStage, ["httpStatus"] = httpStatus, ["cause"] = failureKind };
                publish(leaf, ".failed", failure);
                lock (receiptLock) errors.Add(failure);
                return;
            }
            string sha256;
            using (var memory = new System.IO.MemoryStream(bytes, false))
                sha256 = digest(memory);
            using (var output = Sandbox.IO.File.Open(path(leaf), System.IO.FileMode.CreateNew,
                System.IO.FileAccess.Write, System.IO.FileShare.None))
                output.Write(bytes, 0, bytes.Length);
            var receipt = new Newtonsoft.Json.Linq.JObject { ["fileId"] = leaf, ["name"] = item["name"],
                ["queryId"] = queryId, ["company"] = company, ["bytes"] = bytes.LongLength, ["sha256"] = sha256 };
            publish(leaf, ".ready", receipt);
            lock (receiptLock)
            {
                receipts[leaf] = receipt;
                files.Add(receipt.DeepClone());
            }
        });
    }
    oSuccess = errors.Count == 0;
    oMessage = oSuccess ? "The batch export is complete." : "Some BAQs could not be exported. Inspect the item diagnostics.";
    oJson = new Newtonsoft.Json.Linq.JObject { ["files"] = files, ["errors"] = errors,
        ["peakExports"] = peakExports }.ToString(Newtonsoft.Json.Formatting.None);
}
