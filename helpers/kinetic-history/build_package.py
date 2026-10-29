"""Build the read-only history helper and its plugin declaration."""
from pathlib import Path
import json

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[1]
PLUGIN = REPO / 'crates/herbert-plugins/packages/kinetic-applications-and-layers'
GLOBAL_ID = 'ff896bb9-5bfb-4cfe-a4c0-126880575f43'

def build():
    code = (ROOT / 'src/read-page.cs').read_text(encoding='utf-8')
    signature = []
    for response, values in [(False, [('iRequests', 'System.String'), ('iOffset', 'System.Int32'), ('iPageSize', 'System.Int32')]),
                             (True, [('oSuccess', 'System.Boolean'), ('oMessage', 'System.String'), ('oJson', 'System.String')])]:
        for order, (name, datatype) in enumerate(values, 1):
            signature.append(dict(ArgumentName=name, DataType=datatype, Response=response,
                                  Order=order, ParameterID=order, Optional=False, Description=''))
    versions = []
    # The 2024.2 native reader was compiled and exercised with these same bodies
    # and signatures at 4.3.200. Later targets keep their exact native overrides.
    for version, native in [('4.3.200', None), ('5.1.100', '12.1.100.29'), ('5.2.100', '12.2.100.9')]:
        package = dict(Mode='Backup', Version=version, SystemCode='ERP', LibraryId='HerbertKineticHistory')
        package['Library'] = dict(OriginalID='HerbertKineticHistory', Description='Read paged saved Kinetic history',
            GlobalID=GLOBAL_ID, EpicorVersion=version, Revision=1, Published=False, Private=False,
            Disabled=False, Mode=0, AllowCustomCodeWidgets=False, AllowCustomCodeFunctions=True,
            DirectDBAccess=1, Notes='Read-only temporary history helper. Scope follows the authenticated session.',
            OwnedByCompany='', Owner='', SharedWithGroup='', LibraryMappings=[],
            LibraryReferences=[dict(ReferenceType=0, ReferenceID='Newtonsoft.Json.dll'),
                               dict(ReferenceType=1, ReferenceID='ICE.UXAppVersion', Updatable=False)],
            Functions=[dict(FunctionID='ReadPage', Description='Read one scoped history page', Kind=2,
                RequireTransaction=False, SingleRowMode=False, Private=False, Disabled=False, Invalid=False,
                Thumbnail=None, Notes='', Body=json.dumps(dict(Code=code, Usings='')), FunctionSignature=signature)])
        name = f'HerbertKineticHistory-{version}.efxj'
        for target in [ROOT/name, PLUGIN/'artifacts'/name]:
            target.parent.mkdir(parents=True, exist_ok=True)
            text = json.dumps(package, indent=2)+'\n'
            if version == '4.3.200':
                target.write_bytes(text.encode('utf-8-sig'))
            else:
                target.write_text(text, encoding='utf-8-sig')
        if native is not None:
            versions.append(dict(epicorVersion=native, file='artifacts/'+name))
    artifact = dict(alias='history', purpose='serverWork', coverage='someContent', type='functionLibrary',
        file='artifacts/HerbertKineticHistory-4.3.200.efxj', versionFiles=versions, lifetime='temporary', policy='createInstance',
        requiredFunctions=[dict(function='ReadPage', parameters=[dict(name=s['ArgumentName'], dataType=s['DataType'],
            direction='output' if s['Response'] else 'input') for s in signature])],
        effects=dict(description='Installs a read-only history reader only when history is requested in Server Assisted mode; removes the temporary library after collection.'))
    (PLUGIN/'plugin.json').write_text(json.dumps(dict(artifacts=[artifact]), indent=2)+'\n', encoding='utf-8', newline='')

if __name__ == '__main__':
    build()
