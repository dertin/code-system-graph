"""Capture actual read-only MCP responses on the platform-demo synthetic repos."""
from pathlib import Path
import json, subprocess, shutil, select, time, datetime, os

ROOT = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
BIN = Path(os.environ.get('CSGRAPH_EXAMPLES_BIN', '/opt/procesador/target_workspace/debug/csgraph'))
WORK = OUT / '.work'
WORK.mkdir(exist_ok=True)
FIXTURE = WORK / 'commerce'
if not FIXTURE.exists():
    shutil.copytree(ROOT / 'fixtures/platform-demo', FIXTURE)

def run(args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, text=True, capture_output=True, **kwargs).stdout

assert run([BIN,'--version']).strip() == 'csgraph 1.2.0', 'Use a csgraph 1.2.0 binary'
assert run(['codegraph','--version']).strip() == '1.6.1', 'Use CodeGraph 1.6.1'

# This is synthetic local evidence. Each directory represents a repository.
revisions = {}
for alias, directory in [('web','web'),('api','api'),('tests','tests-python'),('worker','worker-python'),('infra','infra'),('docs','docs')]:
    path = FIXTURE / directory
    if not (path/'.git').exists():
        run(['git','init','-q','-b','example'], cwd=path)
        run(['git','add','.'], cwd=path)
        run(['git','-c','user.name=Example Fixture','-c','user.email=fixture@example.invalid','commit','-qm','Synthetic platform fixture'], cwd=path)
    (path/'.git/info/exclude').write_text('.codegraph/\n')
    revisions[alias] = run(['git','rev-parse','HEAD'], cwd=path).strip()
    if alias in ('web','api','tests','worker') and not (path/'.codegraph').exists():
        start=time.monotonic()
        result=run(['codegraph','init','--yes',path])
        (WORK/f'init-{alias}.log').write_text(result)
        print(f'Indexed {alias}: {time.monotonic()-start:.1f}s', flush=True)

MANIFEST = FIXTURE/'code-system-graph.yaml'
DB = WORK/'graph.db'
scan = json.loads(run([BIN,'scan','--force','--config',MANIFEST,'--database',DB]))
(WORK/'scan.json').write_text(json.dumps(scan,indent=2))
graph = json.loads(run([BIN,'export','--database',DB,'--workspace','commerce-platform','--format','json']))
(WORK/'graph.json').write_text(json.dumps(graph,indent=2))
print('Scanned. Export keys:', list(graph), flush=True)
err = (WORK/'mcp.stderr').open('w')
process = subprocess.Popen([str(BIN),'mcp','--config',str(MANIFEST),'--database',str(DB),'--workspace','commerce-platform','--codegraph','--codegraph-binary',shutil.which('codegraph')], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, text=True, bufsize=1)
seq = 0

def request(method, params):
    global seq
    seq+=1
    payload={'jsonrpc':'2.0','id':seq,'method':method,'params':params}
    process.stdin.write(json.dumps(payload)+'\n'); process.stdin.flush()
    deadline=time.monotonic()+150
    while time.monotonic()<deadline:
        ready,_,_=select.select([process.stdout],[],[],min(1,deadline-time.monotonic()))
        if not ready: continue
        line=process.stdout.readline()
        if not line: raise RuntimeError('MCP process exited: '+(WORK/'mcp.stderr').read_text())
        reply=json.loads(line)
        if reply.get('id')==seq:
            if 'error' in reply: return {'protocol_error':reply['error']}
            return reply['result']
    raise TimeoutError(method)

cases=[]
def call(id, title, question, tool, args):
    start=time.monotonic()
    result=request('tools/call',{'name':tool,'arguments':args})
    case={'id':id,'title':title,'question':question,'tool':tool,'arguments':args,'response':result,'elapsed_ms':round((time.monotonic()-start)*1000)}
    cases.append(case)
    (OUT/'responses.partial.json').write_text(json.dumps(cases,ensure_ascii=False,indent=2))
    print(id,tool,'status=',result.get('structuredContent',{}).get('status'), 'text chars=',sum(len(c.get('text','')) for c in result.get('content',[])),flush=True)
    return result

try:
    initialized=request('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'response-gallery','version':'1.0'}})
    process.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');process.stdin.flush()
    tools=request('tools/list',{})
    (WORK/'tools.json').write_text(json.dumps(tools,indent=2))
    call('broad','Búsqueda amplia','¿Qué está relacionado con orders?', 'query', {'query':'orders','limit':8})
    targeted=call('http','Vínculo HTTP entre repositorios','¿Qué consumidores y tests se vinculan a POST /api/orders?', 'query',{'query':'/api/orders','limit':5})
    call('http-filtered','Misma búsqueda con filtro','¿Qué cambia si busco solo operaciones HTTP?', 'query',{'query':'/api/orders','node_kinds':['http_operation'],'limit':5})
    call('test-query','Localizar el test','¿Dónde está el test test_create_order?', 'query',{'query':'test_create_order','limit':3})
    # IDs are discovered in the actual persisted export, never fabricated.
    data=graph.get('data',graph)
    print('Graph data keys:',list(data),flush=True)
    nodes=data.get('nodes',[])
    if not nodes and 'graph' in data: nodes=data['graph']['nodes']
    def node(label,kind=None,stable_contains=None):
        matches=[n for n in nodes if n['label']==label and (not kind or n['kind']==kind) and (not stable_contains or stable_contains in n['stable_key'])]
        if len(matches)!=1: raise RuntimeError(f'Node selection {label!r}: '+str([(n['label'],n['kind']) for n in matches]))
        return matches[0]['id']
    test=node('python/pytest::test_create_order','test_case')
    operation=node('POST /api/orders','http_operation',':provider:')
    call('test-context','Test y contrato vinculado','¿Qué valida este test y dónde está la evidencia?', 'source_context',{'node_id':test,'evidence_limit':8})
    call('trace','Camino exacto','¿Existe un vínculo confirmado entre el test y POST /api/orders?', 'trace',{'from':test,'to':operation,'max_depth':5})
    call('events','Publicador y suscriptor','¿Quién publica y quién consume orders.created?', 'query',{'query':'orders.created','limit':5})
    tables=[n for n in nodes if n['kind']=='database_table' and n['stable_key']=='table::commerce:orders']
    print('Tables:',[(n['label'],n['id']) for n in tables],flush=True)
    if len(tables)==1: call('database','Tabla compartida','¿Qué repositorios declaran o consultan la tabla orders?', 'source_context',{'node_id':tables[0]['id'],'evidence_limit':8})
    call('web-explore','Código del consumidor y salto a API','¿Cómo llama createOrder al backend y a qué repo puedo continuar?', 'explore',{'repository':'web','query':'createOrder','max_files':2})
    call('api-explore','Implementación de la API','¿Qué devuelve create_order?', 'explore',{'repository':'api','query':'create_order','max_files':2})
    call('test-explore','Assertion del test','¿Qué respuesta espera test_create_order?', 'explore',{'repository':'tests','query':'test_create_order','max_files':2})
    call('missing','Búsqueda sin coincidencias','¿Dónde está refundInvoice?', 'query',{'query':'refundInvoice','limit':5})
    call('bad-scope','Explore sin elegir repo','¿Qué pasa si el agente no indica un repositorio?', 'explore',{'query':'create_order','max_files':2})
    sources={str(p.relative_to(FIXTURE)):p.read_text() for p in [FIXTURE/'web/src/checkout.ts',FIXTURE/'api/src/lib.rs',FIXTURE/'tests-python/tests/test_orders.py',FIXTURE/'worker-python/worker.py']}
    capture={'captured_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'binary':run([BIN,'--version']).strip(),'codegraph':run(['codegraph','--version']).strip(),'source_commit':run(['git','rev-parse','HEAD'],cwd=ROOT).strip(),'origin':'Local build of current source; synthetic platform-demo repositories; real MCP calls, no fake CodeGraph provider.','initialize':initialized,'repo_revisions':revisions,'sources':sources,'cases':cases}
    (OUT/'responses.json').write_text(json.dumps(capture,ensure_ascii=False,indent=2))
    (OUT/'responses.partial.json').unlink(missing_ok=True)
finally:
    process.terminate()
    try: process.wait(timeout=5)
    except subprocess.TimeoutExpired: process.kill();process.wait()
    err.close()
