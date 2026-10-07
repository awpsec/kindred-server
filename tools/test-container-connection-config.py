"""Test the production entrypoint config expression without running services."""
import ast,json,tomllib,types
from pathlib import Path
source=Path(__file__).resolve().parents[1]/'deploy/container-entry.py'
assignments=[n for n in ast.parse(source.read_text()).body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id=='config' for t in n.targets)]
assert len(assignments)==1
expression=compile(ast.Expression(assignments[0].value),str(source),'eval')
for selected,confirmed in [([],[]),(['http://192.168.1.20:9444'],[]),(['http://203.0.113.7:9444'],['http://203.0.113.7:9444']),([],[])]:
 env={'KINDRED_ALLOWED_ORIGINS':json.dumps(selected),'KINDRED_CONFIRMED_HTTP_ORIGINS':json.dumps(confirmed)}
 text=eval(expression,{'json':json,'os':types.SimpleNamespace(environ=env),'url':'http://127.0.0.1:9444'})
 config=tomllib.loads(text)
 assert config['allowed_origins']==selected and config['confirmed_http_origins']==confirmed
 assert config['public_url']=='http://127.0.0.1:9444' and config['profiles']['enabled'] is True
print('production entrypoint: default, legacy private HTTP, confirmed exact HTTP and revoked config preserved; no service or settings written')
