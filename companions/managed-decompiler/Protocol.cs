using System.Text.Json.Serialization;
namespace Herbert.ManagedDecompiler;
internal sealed record InputFile(string Path, long Bytes, string Sha256);
internal sealed record LeaseInput(string Root, string Generation);
internal sealed record Request(int ProtocolVersion, string JobId, string EngineId, InputFile Input, InputFile? Symbols, string SymbolMode, string OutputDirectory, bool ControlChannel, LeaseInput? Lease);
internal sealed record OutputFile(string Path, long Bytes, string Sha256, string Kind, string? LogicalName);
internal sealed record Coverage(int Token, string Kind, string Name, string Outcome, string? Path, string? Detail);
internal sealed record Result(int ProtocolVersion, string JobId, string EngineId, string InputSha256, string Outcome, string SymbolDisposition, string? SymbolSha256, string[] UnresolvedReferences, Coverage[] Coverage, OutputFile[] Files, string[] Diagnostics, long ElapsedMilliseconds);
[JsonSourceGenerationOptions(PropertyNamingPolicy=JsonKnownNamingPolicy.CamelCase, WriteIndented=true, UnmappedMemberHandling=JsonUnmappedMemberHandling.Disallow)]
[JsonSerializable(typeof(Request))][JsonSerializable(typeof(Result))]
internal partial class ProtocolJson : JsonSerializerContext;
