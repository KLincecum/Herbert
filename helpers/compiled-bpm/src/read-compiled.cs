// Caller-owned ordinary BPM policy. Read only; never compiles or executes a directive.
oSuccess=false;oMessage="";oDS=new System.Data.DataSet("BpmCompiledCapture");
try {
 if(String.IsNullOrEmpty(iCaller)||!String.Equals(iCaller,Session.UserID,StringComparison.Ordinal)||!String.Equals(iCompany,Session.CompanyID,StringComparison.Ordinal))throw new Exception("CALLER_COMPANY_MISMATCH");
 var selected=Newtonsoft.Json.Linq.JArray.Parse(iOwners);
 var owners=new System.Collections.Generic.Dictionary<string,Newtonsoft.Json.Linq.JObject>(StringComparer.Ordinal);
 foreach(var item in selected){
  string source=(string)item["source"],code=(string)item["method"];
  if((source!="BO"&&source!="DB")||String.IsNullOrWhiteSpace(code))throw new Exception("Explicit ordinary BPM owners are required.");
  string key=source+":"+code;if(owners.ContainsKey(key))throw new Exception("Repeated BPM owner.");
  owners.Add(key,new Newtonsoft.Json.Linq.JObject{{"Source",source},{"BpMethodCode",code}});
 }
 if(owners.Count==0)throw new Exception("Explicit BPM owners are required.");
 Func<Newtonsoft.Json.Linq.JObject,string,string,Newtonsoft.Json.Linq.JArray> witness=(ds,source,code)=>{
  var result=new Newtonsoft.Json.Linq.JArray();
  foreach(var table in new[]{"BpMethod","BpArgument","BpDirective"}){
   var rows=ds[table] as Newtonsoft.Json.Linq.JArray;if(rows==null)throw new Exception("Native definition table missing.");
   foreach(var row in rows.Where(r=>(string)r["Source"]==source&&(string)r["BpMethodCode"]==code).OrderBy(r=>(string)r["SysRowID"],StringComparer.Ordinal)){
    if(row["SysRowID"]==null||row["SysRevID"]==null)throw new Exception("Native revision witness missing.");
    result.Add(new Newtonsoft.Json.Linq.JObject{{"Table",table},{"SysRowID",row["SysRowID"].DeepClone()},{"SysRevID",row["SysRevID"].DeepClone()}});
   }
  }
  return result;
 };
 Action<bool> readDefinitions=after=>this.CallService<Ice.Contracts.BpMethodSvcContract>(svc=>{
  foreach(var source in new[]{"BO","DB"}){
   var codes=owners.Values.Where(o=>(string)o["Source"]==source).Select(o=>(string)o["BpMethodCode"]).ToArray();if(codes.Length==0)continue;
   var filter="SystemFlag = 0 AND ("+String.Join(" OR ",codes.Select(code=>"BpMethodCode = '"+code.Replace("'","''")+"'"))+")";
   bool more;var ds=Newtonsoft.Json.Linq.JObject.FromObject(svc.GetRowsEx(source,filter,"",0,1,out more));if(more)throw new Exception("Native owner read is incomplete.");
   foreach(var code in codes){
    var owner=owners[source+":"+code];var rows=((Newtonsoft.Json.Linq.JArray)ds["BpMethod"]).Where(r=>(string)r["Source"]==source&&(string)r["BpMethodCode"]==code).ToArray();
    if(rows.Length!=1||(bool?)rows[0]["SystemFlag"]!=false)throw new Exception("Selected BPM owner is not visible.");
    var method=rows[0];var parts=code.Split('.');string assembly,folder;
    if(source=="BO"){
     if(parts.Length!=4||(string)method["ObjectNS"]!=parts[1]||(string)method["BusinessObject"]!=parts[2]||(string)method["Name"]!=parts[3])throw new Exception("Native BO identity differs.");
     folder="BO";assembly=parts[0]+".Bpm."+String.Join(".",parts.Skip(1));
    }else{
     if(parts.Length!=3||parts[2]!="Update"||(string)method["BusinessObject"]!=parts[1]||(string)method["Name"]!="Update")throw new Exception("Native DB identity differs.");
     folder="DT";assembly=parts[0]+"."+parts[1]+".Bpm.Triggers";
    }
    owner["Folder"]=folder;owner["Assembly"]=assembly;owner["DefinitionWitness"+(after?"After":"")]=witness(ds,source,code).ToString(Newtonsoft.Json.Formatting.None);
    var visible=((Newtonsoft.Json.Linq.JArray)ds["BpDirective"]).Where(r=>(string)r["Source"]==source&&(string)r["BpMethodCode"]==code).Select(r=>(Guid)r["DirectiveID"]).ToList();
    // A compiled assembly can contain more than the caller-visible projection.
    // Refuse its bytes if any stored directive owner is hidden from this caller.
    var stored=Db.BpDirective.Where(r=>r.Source==source&&r.BpMethodCode==code).Select(r=>r.DirectiveID).ToList();
    owner["ScopeComplete"+(after?"After":"")]=stored.Count==visible.Count&&stored.All(id=>visible.Contains(id));
   }
  }
 });
 readDefinitions(false);
 var folderNames=owners.Values.Select(o=>(string)o["Folder"]).Distinct().ToList();var names=owners.Values.Select(o=>(string)o["Assembly"]).Distinct().ToList();
 if(names.Count!=owners.Count)throw new Exception("Ambiguous ordinary BPM assembly owners.");
 var before=Db.CustomizationStore.Where(r=>folderNames.Contains(r.Folder)&&names.Contains(r.Assembly)).Select(r=>new{r.Folder,r.Assembly,r.Version,r.Info,r.ModifiedOn,r.ForcePdbLoad,r.SysRowID,r.SysRevID}).ToList();
 if(before.Any(r=>!owners.Values.Any(o=>String.Equals(r.Folder,(string)o["Folder"],StringComparison.Ordinal)&&String.Equals(r.Assembly,(string)o["Assembly"],StringComparison.Ordinal))))throw new Exception("UNEXPECTED_NATIVE_STORE_SELECTOR");
 var targets=owners.Values.Where(o=>(string)o["Source"]=="BO").Select(o=>(string)o["BpMethodCode"]).ToList();
 var dbNames=owners.Values.Where(o=>(string)o["Source"]=="DB").Select(o=>(string)o["Assembly"]).ToList();
 var regs=Db.Customization.Where(r=>(r.Type=="BPM.BO"&&targets.Contains(r.Target))||(r.Type=="BPM.DT"&&dbNames.Contains(r.Assembly))).Select(r=>new{r.Type,r.Target,r.Scope,r.Project,r.Disabled,r.Revision,r.SysRevID,r.SysRowID,r.Assembly,r.Version,r.TypeName,r.SystemFlag}).ToList();
 Func<Newtonsoft.Json.Linq.JObject,Newtonsoft.Json.Linq.JArray> registration=owner=>new Newtonsoft.Json.Linq.JArray(regs.Where(r=>(string)owner["Source"]=="BO"?r.Type=="BPM.BO"&&String.Equals(r.Target,(string)owner["BpMethodCode"],StringComparison.Ordinal):r.Type=="BPM.DT"&&String.Equals(r.Assembly,(string)owner["Assembly"],StringComparison.Ordinal)).OrderBy(r=>r.Scope,StringComparer.Ordinal).ThenBy(r=>r.Target,StringComparer.Ordinal).Select(r=>Newtonsoft.Json.Linq.JObject.FromObject(r)));
 foreach(var owner in owners.Values){
  var rows=registration(owner);owner["Registration"]=rows.ToString(Newtonsoft.Json.Formatting.None);
  bool valid=true;
  foreach(var r in rows){
   if((string)r["Assembly"]!=(string)owner["Assembly"])valid=false;
   if((string)owner["Source"]=="BO"){
    var parts=((string)owner["BpMethodCode"]).Split('.');if((string)r["Scope"]!=String.Join(":",parts.Take(3))||(string)r["Project"]!=(string)owner["BpMethodCode"])valid=false;
   }else{
    var table=((string)owner["BpMethodCode"]).Split('.')[1];string scope=(string)r["Scope"],suffix=scope=="##DT##"?".Bpm.InTranTrigger":scope=="##CT##"?".Bpm.PostTranTrigger":"";
    if(suffix==""||!((string)r["Target"]).EndsWith("."+table+suffix,StringComparison.Ordinal))valid=false;
   }
  }
  owner["AssociationValid"]=valid;
 }
 var permitted=owners.Values.Where(o=>(bool)o["ScopeComplete"]&&(bool)o["AssociationValid"]).Select(o=>(string)o["Folder"]+"|"+(string)o["Assembly"]).ToList();
 var selectedRows=before.Where(r=>permitted.Contains(r.Folder+"|"+r.Assembly)).Select(r=>r.SysRowID).ToList();
 var bytes=Db.CustomizationStore.Where(r=>selectedRows.Contains(r.SysRowID)).Select(r=>new{r.Folder,r.Assembly,r.Version,r.SysRowID,r.SysRevID,r.Data,r.Symbols}).ToList();
 var afterRows=Db.CustomizationStore.Where(r=>folderNames.Contains(r.Folder)&&names.Contains(r.Assembly)).Select(r=>new{r.Folder,r.Assembly,r.Version,r.SysRevID}).ToList();
 var afterRegs=Db.Customization.Where(r=>(r.Type=="BPM.BO"&&targets.Contains(r.Target))||(r.Type=="BPM.DT"&&dbNames.Contains(r.Assembly))).Select(r=>new{r.Type,r.Target,r.Scope,r.Project,r.Disabled,r.Revision,r.SysRevID,r.SysRowID,r.Assembly,r.Version,r.TypeName,r.SystemFlag}).ToList();
 readDefinitions(true);
 var tableResult=new System.Data.DataTable("Assemblies");
 foreach(var name in new[]{"Source","BpMethodCode","Folder","Assembly","Version","Info","ModifiedOn","SysRowID","SysRevID","BinarySysRevID","AfterSysRevID","SymbolDisposition","ModuleVersionId","AssemblyFullName"})tableResult.Columns.Add(name,typeof(string));
 tableResult.Columns.Add("ForcePdbLoad",typeof(bool));tableResult.Columns.Add("Data",typeof(byte[]));tableResult.Columns.Add("Symbols",typeof(byte[]));
 foreach(var owner in owners.Values){
  string folder=(string)owner["Folder"],assembly=(string)owner["Assembly"];
  owner["BeforeRows"]=before.Count(r=>r.Folder==folder&&r.Assembly==assembly);owner["AfterRows"]=afterRows.Count(r=>r.Folder==folder&&r.Assembly==assembly);
  owner["RegistrationAfter"]=new Newtonsoft.Json.Linq.JArray(afterRegs.Where(r=>(string)owner["Source"]=="BO"?r.Type=="BPM.BO"&&String.Equals(r.Target,(string)owner["BpMethodCode"],StringComparison.Ordinal):r.Type=="BPM.DT"&&String.Equals(r.Assembly,assembly,StringComparison.Ordinal)).OrderBy(r=>r.Scope,StringComparer.Ordinal).ThenBy(r=>r.Target,StringComparer.Ordinal).Select(r=>Newtonsoft.Json.Linq.JObject.FromObject(r))).ToString(Newtonsoft.Json.Formatting.None);
  foreach(var row in before.Where(r=>String.Equals(r.Folder,folder,StringComparison.Ordinal)&&String.Equals(r.Assembly,assembly,StringComparison.Ordinal)).OrderBy(r=>r.Version,StringComparer.Ordinal)){
   var payload=bytes.SingleOrDefault(r=>r.SysRowID==row.SysRowID&&String.Equals(r.Folder,folder,StringComparison.Ordinal)&&String.Equals(r.Assembly,assembly,StringComparison.Ordinal)&&String.Equals(r.Version,row.Version,StringComparison.Ordinal));
   var current=afterRows.SingleOrDefault(r=>String.Equals(r.Folder,folder,StringComparison.Ordinal)&&String.Equals(r.Assembly,assembly,StringComparison.Ordinal)&&String.Equals(r.Version,row.Version,StringComparison.Ordinal));
   byte[] dll=payload==null?null:payload.Data,pdb=payload==null?null:payload.Symbols;string disposition=pdb==null?"absent":"unsupported-unused",mvid="",fullName="";
   if(dll!=null){try{using(var stream=new System.IO.MemoryStream(dll))using(var pe=new System.Reflection.PortableExecutable.PEReader(stream)){
    var metadata=System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);mvid=metadata.GetGuid(metadata.GetModuleDefinition().Mvid).ToString();var identity=metadata.GetAssemblyDefinition();fullName=metadata.GetString(identity.Name)+", Version="+identity.Version.ToString();
    if(pdb!=null){try{using(var symbolStream=new System.IO.MemoryStream(pdb))using(var provider=System.Reflection.Metadata.MetadataReaderProvider.FromPortablePdbStream(symbolStream)){
     var sm=provider.GetMetadataReader();if(sm.DebugMetadataHeader==null)throw new System.BadImageFormatException();var symbolID=new System.Reflection.Metadata.BlobContentId(sm.DebugMetadataHeader.Id);
     disposition=pe.ReadDebugDirectory().Any(e=>e.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.CodeView&&e.Stamp==symbolID.Stamp&&pe.ReadCodeViewDebugDirectoryData(e).Guid==symbolID.Guid&&pe.ReadCodeViewDebugDirectoryData(e).Age==1)?"matched-portable":"mismatched-unused";
    }}catch(System.BadImageFormatException){disposition="unsupported-unused";}}
    if(disposition!="matched-portable"&&pe.ReadDebugDirectory().Any(e=>e.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.EmbeddedPortablePdb))disposition+=";embedded-candidate";
   }}catch(System.BadImageFormatException){disposition="binary-invalid;"+disposition;}}
   tableResult.Rows.Add((string)owner["Source"],(string)owner["BpMethodCode"],folder,assembly,row.Version,row.Info,row.ModifiedOn.ToString("o",System.Globalization.CultureInfo.InvariantCulture),row.SysRowID.ToString(),Convert.ToBase64String(row.SysRevID),payload==null?"":Convert.ToBase64String(payload.SysRevID),current==null?"":Convert.ToBase64String(current.SysRevID),disposition,mvid,fullName,row.ForcePdbLoad,(object)dll??DBNull.Value,(object)pdb??DBNull.Value);
  }
 }
 oDS.Tables.Add(tableResult);
 var ownerTable=new System.Data.DataTable("Owners");foreach(var name in new[]{"Source","BpMethodCode","Folder","Assembly","DefinitionWitness","DefinitionWitnessAfter","Registration","RegistrationAfter"})ownerTable.Columns.Add(name,typeof(string));
 foreach(var name in new[]{"ScopeComplete","ScopeCompleteAfter","AssociationValid"})ownerTable.Columns.Add(name,typeof(bool));foreach(var name in new[]{"BeforeRows","AfterRows"})ownerTable.Columns.Add(name,typeof(int));
 foreach(var owner in owners.Values)ownerTable.Rows.Add((string)owner["Source"],(string)owner["BpMethodCode"],(string)owner["Folder"],(string)owner["Assembly"],(string)owner["DefinitionWitness"],(string)owner["DefinitionWitnessAfter"],(string)owner["Registration"],(string)owner["RegistrationAfter"],(bool)owner["ScopeComplete"],(bool)owner["ScopeCompleteAfter"],(bool)owner["AssociationValid"],(int)owner["BeforeRows"],(int)owner["AfterRows"]);
 oDS.Tables.Add(ownerTable);
 var scopeTable=new System.Data.DataTable("Scope");scopeTable.Columns.Add("Caller",typeof(string));scopeTable.Columns.Add("Company",typeof(string));scopeTable.Rows.Add(Session.UserID,Session.CompanyID);oDS.Tables.Add(scopeTable);oSuccess=true;
}catch(Exception error){oMessage=error.Message;}
