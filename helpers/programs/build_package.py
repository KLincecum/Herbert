"""Update both native helper packages from the reviewed single read-only source."""
from pathlib import Path
import json
root=Path(__file__).resolve().parent
plugin=root.parents[1]/'crates/herbert-plugins/packages/programs-and-extensions/artifacts'
code=(root/'src/read-internal-metadata.cs').read_text(encoding='utf-8')
for version in ('5.1.100','5.2.100'):
 file=root/('HerbertPrograms-'+version+'.efxj')
 package=json.loads(file.read_text(encoding='utf-8-sig'));library=package['Library'];assert package['LibraryId']=='HerbertPrograms';assert len(library['Functions'])==1
 function=library['Functions'][0];assert function['FunctionID']=='ReadInternalMetadata';function['Body']=json.dumps({'Code':code,'Usings':''})
 payload=json.dumps(package,indent=2)+'\n'
 for target in (file,plugin/file.name):target.write_text(payload,encoding='utf-8-sig')
 print(version,'single-source read-only helper updated')
