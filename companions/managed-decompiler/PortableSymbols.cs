using System.Reflection.Metadata;
using ICSharpCode.Decompiler.DebugInfo;
namespace Herbert.ManagedDecompiler;
// Reads only the supplied, identity-validated portable metadata stream. No source
// paths, Source Link, symbol servers or external files are opened.
internal sealed class PortableSymbols(MetadataReader metadata) : IDebugInfoProvider
{
    public string Description => "Identity-validated portable symbols (local variables and sequence points)";
    public string SourceFileName => "verified-symbols.pdb";
    public IList<Variable> GetVariables(MethodDefinitionHandle method)
    {
        List<Variable> result = [];
        foreach (var s in metadata.GetLocalScopes(method))
            foreach (var h in metadata.GetLocalScope(s).GetLocalVariables())
            { var v = metadata.GetLocalVariable(h); result.Add(new(v.Index, metadata.GetString(v.Name))); }
        return result;
    }
    public bool TryGetName(MethodDefinitionHandle method, int index, out string name)
    {
        foreach (var v in GetVariables(method)) if (v.Index == index) { name = v.Name; return true; }
        name = ""; return false;
    }
    public IList<ICSharpCode.Decompiler.DebugInfo.SequencePoint> GetSequencePoints(MethodDefinitionHandle method)
    {
        List<ICSharpCode.Decompiler.DebugInfo.SequencePoint> result = [];
        foreach (var p in metadata.GetMethodDebugInformation(method).GetSequencePoints())
            result.Add(new() { Offset=p.Offset, StartLine=p.StartLine, EndLine=p.EndLine, StartColumn=p.StartColumn, EndColumn=p.EndColumn, DocumentUrl=p.Document.IsNil ? "" : metadata.GetString(metadata.GetDocument(p.Document).Name) });
        return result;
    }
    public bool TryGetExtraTypeInfo(MethodDefinitionHandle method, int index, out PdbExtraTypeInfo extraTypeInfo)
    {
        extraTypeInfo=default; LocalVariableHandle selected=default;
        foreach (var s in metadata.GetLocalScopes(method))
            foreach (var h in metadata.GetLocalScope(s).GetLocalVariables())
                if (metadata.GetLocalVariable(h).Index==index) { selected=h; break; }
        if (selected.IsNil) return false;
        foreach (var h in metadata.GetCustomDebugInformation(selected))
        {
            var info=metadata.GetCustomDebugInformation(h);var kind=metadata.GetGuid(info.Kind);var bytes=metadata.GetBlobBytes(info.Value);
            if (kind==new Guid("ED9FDF71-8879-4747-8ED3-FE5EDE3CE710"))
            {
                var names=new List<string>();var start=0;
                for(var i=0;i<bytes.Length;i++)if(bytes[i]==0){names.Add(System.Text.Encoding.UTF8.GetString(bytes,start,i-start));start=i+1;}
                if(start!=bytes.Length)throw new BadImageFormatException("Unterminated portable tuple-name annotation");
                extraTypeInfo.TupleElementNames=names.ToArray();
            }
            if (kind==new Guid("83C563C4-B4F3-47D5-B824-BA5441477EA8"))
            {
                var flags=new bool[checked(bytes.Length*8)];for(var i=0;i<bytes.Length;i++)for(var bit=0;bit<8;bit++)flags[i*8+bit]=(bytes[i]&(1<<bit))!=0;
                extraTypeInfo.DynamicFlags=flags;
            }
        }
        return extraTypeInfo.TupleElementNames!=null || extraTypeInfo.DynamicFlags!=null;
    }
}
