using System.Diagnostics;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using ICSharpCode.Decompiler;
using ICSharpCode.Decompiler.CSharp;
using ICSharpCode.Decompiler.Metadata;
namespace Herbert.ManagedDecompiler;
internal static class Program
{
    const string Engine = "ilspy-11.1.0.9782-herbert-v1";
    static readonly CancellationTokenSource Cancellation = new();
    static int Main(string[] args)
    {
        if (args.Length == 1 && args[0] == "--protocol") { Console.WriteLine("{\"protocolVersion\":1,\"engineId\":\""+Engine+"\",\"resolverPolicy\":\"target-only-v1\",\"aot\":true,\"controlChannel\":\"line-or-eof-v1\",\"generationLeases\":\"os-lock-token-v1\",\"symbolModes\":[\"none\",\"externalPortable\",\"embeddedPortable\"]}"); return 0; }
        if (args.Length != 1) { Console.Error.WriteLine("Pass one host-created request file."); return 2; }
        Console.CancelKeyPress += (_, e) => { e.Cancel = true; Cancellation.Cancel(); };
        try {
            var request=JsonSerializer.Deserialize(File.ReadAllBytes(args[0]), ProtocolJson.Default.Request) ?? throw new InvalidDataException("Missing request");
            if(request.ControlChannel) _ = Task.Run(async () => { var line=await Console.In.ReadLineAsync(); if (line=="cancel" || line==null) Cancellation.Cancel(); });
            return Execute(request);
        }
        catch (Exception e) { Console.Error.WriteLine(e.ToString()); return 1; }
    }
    static FileStream Verified(InputFile input)
    {
        var file = new FileStream(input.Path, FileMode.Open, FileAccess.Read, FileShare.Read);
        if (file.Length != input.Bytes || Convert.ToHexStringLower(SHA256.HashData(file)) != input.Sha256)
        { file.Dispose(); throw new InvalidDataException("Input length/hash mismatch"); }
        file.Position=0; return file;
    }
    static int Execute(Request request)
    {
        if (request.ProtocolVersion != 1 || request.EngineId != Engine || request.JobId.Length == 0 || !(request.SymbolMode is "none" or "externalPortable" or "embeddedPortable"))
            throw new InvalidDataException("Unsupported protocol/engine/job identity");
        using var ownership=request.Lease==null?null:new WorkerLease(request.Lease);
        Cancellation.Token.ThrowIfCancellationRequested();
        var timer = Stopwatch.StartNew();
        using var sink = new ConfinedSink(request.OutputDirectory);
        List<string> diagnostics=[]; List<Coverage> coverage=[];
        using var input=Verified(request.Input);
        using var pe=new PEFile("verified-input.dll", input, PEStreamOptions.PrefetchEntireImage);
        if (!pe.Metadata.IsAssembly) throw new BadImageFormatException("Managed input is a module, not an assembly");
        if ((pe.Reader.PEHeaders.CorHeader!.Flags & CorFlags.ILOnly) == 0) throw new BadImageFormatException("Mixed/native managed input is unsupported");
        var resolver = new TargetOnlyResolver();
        var settings = new DecompilerSettings { ThrowOnAssemblyResolveErrors=false };
        var decompiler = new CSharpDecompiler(pe, resolver, settings) { CancellationToken=Cancellation.Token, DocumentationProvider=new NoDocumentation() };
        MetadataReaderProvider? symbols=null; FileStream? symbolStream=null;
        var symbolDisposition="absent"; string? symbolHash=null;
        try
        {
            if (request.SymbolMode == "externalPortable" && request.Symbols != null)
            {
                try
                {
                    symbolStream=Verified(request.Symbols);
                    symbols=MetadataReaderProvider.FromPortablePdbStream(symbolStream, MetadataStreamOptions.LeaveOpen);
                    if (!Matches(pe.Reader, symbols.GetMetadataReader()))
                    { diagnostics.Add("External symbols do not match the DLL CodeView GUID/stamp; unused."); symbols.Dispose(); symbols=null; symbolDisposition="mismatched-unused"; }
                    else { symbolDisposition="matched-used"; symbolHash=request.Symbols.Sha256; }
                }
                catch (BadImageFormatException e) { diagnostics.Add("External symbols unsupported: "+e.Message); symbolDisposition="unsupported-unused"; symbols?.Dispose(); symbols=null; }
            }
            if (request.SymbolMode == "embeddedPortable" && symbols == null)
            {
                foreach (var entry in pe.Reader.ReadDebugDirectory().Where(e=>e.Type==DebugDirectoryEntryType.EmbeddedPortablePdb))
                {
                    try {
                        var embedded=pe.Reader.ReadEmbeddedPortablePdbDebugDirectoryData(entry);
                        if (Matches(pe.Reader,embedded.GetMetadataReader())) { symbols=embedded; symbolDisposition="embedded-matched-used"; break; }
                        embedded.Dispose(); symbolDisposition="embedded-mismatched-unused"; diagnostics.Add("Embedded symbols do not match CodeView identity; unused.");
                    }
                    catch (BadImageFormatException e) { symbolDisposition="embedded-unsupported-unused"; diagnostics.Add("Embedded symbols unsupported: "+e.Message); }
                }
            }
            if (symbols != null)
            {
                decompiler.DebugInfoProvider=new PortableSymbols(symbols.GetMetadataReader());

            }
            foreach (var handle in pe.Metadata.TypeDefinitions)
            {
                Cancellation.Token.ThrowIfCancellationRequested();
                var type=pe.Metadata.GetTypeDefinition(handle);
                if (!type.GetDeclaringType().IsNil) continue;
                var token=MetadataTokens.GetToken(handle); var name=pe.Metadata.GetString(type.Namespace)+"."+pe.Metadata.GetString(type.Name);
                if (pe.Metadata.GetString(type.Name)=="<Module>") {
                    AddMembers(pe.Metadata,handle,null,"metadata-only","Module/global members are retained in the original DLL; no standalone C# representation is claimed.",coverage);
                    if(type.GetMethods().Count!=0 || type.GetFields().Count!=0)diagnostics.Add("Module/global members require the original DLL; C# reconstruction is unavailable.");
                    continue;
                }
                var path="type-"+token.ToString("x8")+".cs"; var outcome="reconstructed-or-transformed"; string? detail=null;
                try
                {
                    var code=decompiler.DecompileTypesAsString([handle]);
                    sink.WriteText(path,"// Reconstructed from captured IL; original source and buildability are not guaranteed.\n"+code,"source",name);
                    if (code.Contains("Error decompiling") || code.Contains("ILSpy could not decompile") || code.Contains("Unknown result type") || code.Contains("Invalid IL") || code.Contains("Could not resolve"))
                    { outcome="partial"; detail="Engine emitted fidelity/IL diagnostics in this file"; diagnostics.Add(name+": "+detail); }
                }
                catch (OperationCanceledException) { throw; }
                catch (Exception e) { outcome="failed"; detail=e.ToString(); diagnostics.Add(name+": "+detail); path=null; }
                AddMembers(pe.Metadata, handle, path, outcome, detail, coverage);
            }
            foreach (var handle in pe.Metadata.ManifestResources)
            {
                Cancellation.Token.ThrowIfCancellationRequested();
                var resource=pe.Metadata.GetManifestResource(handle);var token=MetadataTokens.GetToken(handle);var name=pe.Metadata.GetString(resource.Name);
                if (!resource.Implementation.IsNil) { coverage.Add(new(token,"resource",name,"unavailable",null,"External resource is not downloaded"));diagnostics.Add("External resource unavailable: "+name);continue; }
                try
                {
                    var dir=pe.Reader.PEHeaders.CorHeader.ResourcesDirectory;
                    var reader=pe.Reader.GetSectionData(dir.RelativeVirtualAddress).GetReader();
                    if (reader.Length < 4 || resource.Offset > int.MaxValue || resource.Offset > (uint)reader.Length - 4) throw new BadImageFormatException("Resource offset outside PE section");
                    reader.Offset=(int)resource.Offset;var length=reader.ReadUInt32();
                    if (length > reader.RemainingBytes) throw new BadImageFormatException("Resource length outside PE section");
                    var path="resource-"+token.ToString("x8")+".bin";
                    using var stream=new MemoryStream(reader.ReadBytes(checked((int)length)));
                    sink.Write(path,stream,"raw-resource",name);coverage.Add(new(token,"resource",name,"raw-preserved",path,"No object/resource deserialization"));
                }
                catch(Exception e) { coverage.Add(new(token,"resource",name,"failed",null,e.Message));diagnostics.Add(name+": "+e.Message); }
            }
            var project=new StringBuilder("<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework><EnableDefaultCompileItems>false</EnableDefaultCompileItems><EnableDefaultEmbeddedResourceItems>false</EnableDefaultEmbeddedResourceItems><AllowUnsafeBlocks>true</AllowUnsafeBlocks></PropertyGroup><ItemGroup>");
            foreach(var f in sink.Files) project.Append(f.Kind=="source"?"<Compile Include=\"":"<None Include=\"").Append(f.Path).Append("\" />");
            project.Append("</ItemGroup></Project>\n");
            sink.WriteText("Reconstructed.csproj",project.ToString(),"project");
            sink.WriteText("README.txt","Reconstructed C# from captured IL. No dependencies, sources or symbols were downloaded. Framework in the project is an inspection scaffold, not a claim of the original target or buildability. Raw resource bytes and exact logical names are listed in result.json. See coverage and diagnostics.\n","notice");
            var missing=resolver.Missing.Distinct().Order(StringComparer.Ordinal).ToArray();
            // Enumerate declared dependencies even when a transform did not ask the resolver.
            foreach(var h in pe.Metadata.AssemblyReferences) resolver.Missing.Add(new ICSharpCode.Decompiler.Metadata.AssemblyReference(pe,h).FullName);
            missing=resolver.Missing.Distinct().Order(StringComparer.Ordinal).ToArray();
            var partial=missing.Length!=0 || diagnostics.Count!=0 || coverage.Any(x=>x.Outcome is "failed" or "partial" or "unavailable");
            var result=new Result(1,request.JobId,Engine,request.Input.Sha256,partial?"partial":"complete",symbolDisposition,symbolHash,missing,coverage.ToArray(),sink.Files.ToArray(),diagnostics.ToArray(),timer.ElapsedMilliseconds);
            sink.WriteText("result.json",JsonSerializer.Serialize(result,ProtocolJson.Default.Result),"result");
            return 0;
        }
        finally { symbols?.Dispose(); symbolStream?.Dispose(); }
    }
    static bool Matches(PEReader pe, MetadataReader pdb)
    {
        var header=pdb.DebugMetadataHeader??throw new BadImageFormatException("The supplied symbol metadata has no portable PDB header.");
        var id=new BlobContentId(header.Id);
        return pe.ReadDebugDirectory().Any(e=>e.Type==DebugDirectoryEntryType.CodeView && e.Stamp==id.Stamp && pe.ReadCodeViewDebugDirectoryData(e).Guid==id.Guid && pe.ReadCodeViewDebugDirectoryData(e).Age==1);
    }
    static void AddMembers(MetadataReader m, TypeDefinitionHandle h,string? path,string outcome,string? detail,List<Coverage> result)
    {
        var t=m.GetTypeDefinition(h);result.Add(new(MetadataTokens.GetToken(h),"type",m.GetString(t.Namespace)+"."+m.GetString(t.Name),outcome,path,detail));
        foreach(var x in t.GetMethods())result.Add(new(MetadataTokens.GetToken(x),"method",m.GetString(m.GetMethodDefinition(x).Name),outcome,path,detail));
        foreach(var x in t.GetFields())result.Add(new(MetadataTokens.GetToken(x),"field",m.GetString(m.GetFieldDefinition(x).Name),outcome,path,detail));
        foreach(var x in t.GetProperties())result.Add(new(MetadataTokens.GetToken(x),"property",m.GetString(m.GetPropertyDefinition(x).Name),outcome,path,detail));
        foreach(var x in t.GetEvents())result.Add(new(MetadataTokens.GetToken(x),"event",m.GetString(m.GetEventDefinition(x).Name),outcome,path,detail));
        foreach(var x in t.GetNestedTypes())AddMembers(m,x,path,outcome,"Nested/compiler-generated members can be transformed into containing C# constructs; per-member source equivalence is not claimed.",result);
    }
}
internal sealed class TargetOnlyResolver : IAssemblyResolver
{
    public readonly System.Collections.Concurrent.ConcurrentBag<string> Missing=[];
    public MetadataFile? Resolve(IAssemblyReference r) { Missing.Add(r.FullName);return null; }
    public MetadataFile? ResolveModule(MetadataFile m,string name) { Missing.Add("module:"+name);return null; }
    public Task<MetadataFile?> ResolveAsync(IAssemblyReference r)=>Task.FromResult(Resolve(r));
    public Task<MetadataFile?> ResolveModuleAsync(MetadataFile m,string name)=>Task.FromResult(ResolveModule(m,name));
    public IDisposable? BeginSnapshot()=>null;
}

internal sealed class NoDocumentation : ICSharpCode.Decompiler.Documentation.IDocumentationProvider
{ public string? GetDocumentation(ICSharpCode.Decompiler.TypeSystem.IEntity entity)=>null; }
