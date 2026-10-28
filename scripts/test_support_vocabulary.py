from pathlib import Path
import importlib.util
p=Path(__file__).parent/'generate_support_vocabulary.py';spec=importlib.util.spec_from_file_location('support_generator',p);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
source='''struct X { #[cfg(test)] field: bool } fn live(){ "NATIVE_REFUSAL"; }
#[cfg(test)] mod tests { const SECRET: &str=r##"{ hidden }"##; /* outer /* { } */ } */ fn t(){"CANARY_PRIVATE";} }
fn after(){ "TokenResource"; }
'''
clean=m.production_source(source);assert 'NATIVE_REFUSAL' in clean and 'TokenResource' in clean and 'CANARY_PRIVATE' not in clean and 'hidden' not in clean
root=Path(__file__).resolve().parents[1];words,messages=m.generate(root)
for key in ['NATIVE_REFUSAL','TokenResource','ExportSsrsRdl','basicApiKey','retryAttempts']:assert key in words,key
assert 'CONFIDENTIAL_PRODUCT_AND_PASSWORD_CANARY' not in words and 'secretField' not in words
print('Production labels retained after inline cfg(test); test modules/canaries excluded; all critical built-in labels present',len(words),len(messages))
