"""Measure the captured payloads without modifying them or invoking an LLM."""
from collections import Counter
from pathlib import Path
import hashlib
import json
import re

ROOT = Path(__file__).resolve().parent

def compact(value):
    return json.dumps(value, ensure_ascii=False, separators=(',', ':'))

def objects(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from objects(child)
    elif isinstance(value, list):
        for child in value:
            yield from objects(child)

def repetition(items):
    serialized = [compact(x) for x in items]
    unique = set(serialized)
    return {
        'occurrences': len(serialized),
        'distinct_objects': len(unique),
        'repeated_occurrences': len(serialized)-len(unique),
        'repeated_object_chars': sum(map(len, serialized))-sum(map(len, unique)),
    }

raw = (ROOT/'responses.json').read_bytes()
capture = json.loads(raw)
page = (ROOT/'index.html').read_text()
embedded = json.loads(re.search(r'<script id="capture-data" type="application/json">(.*?)</script>', page, re.S).group(1))
assert len(embedded['cases']) == len(capture['cases'])
for original, shown in zip(capture['cases'], embedded['cases']):
    assert all(original[k] == shown[k] for k in ('id', 'arguments', 'response'))

cases=[]
for c in capture['cases']:
    result=c['response']
    text='\n'.join(x.get('text','') for x in result['content'])
    structured=result['structuredContent']
    data=structured.get('data') or {}
    records=list(objects(structured))
    entities=[x for x in records if {'node_id','stable_key','kind'}.issubset(x)]
    relations=[x for x in records if {'edge_id','source','target'}.issubset(x)]
    evidence=[x for x in records if 'evidence_id' in x]
    field_counts=Counter(k for x in records for k in x)
    json_chars=len(compact(structured))
    cases.append({
        'id':c['id'], 'tool':c['tool'], 'arguments':c['arguments'],
        'status':structured['status'], 'freshness':structured['freshness'],
        'text_chars':len(text), 'text_bytes':len(text.encode()),
        'structured_chars':json_chars, 'structured_bytes':len(compact(structured).encode()),
        'combined_content_chars':len(text)+json_chars,
        'call_tool_result_bytes':len(compact(result).encode()),
        'json_to_text_ratio':round(json_chars/len(text),2),
        'data_field_chars':{k:len(compact(v)) for k,v in data.items()},
        'entities':repetition(entities), 'relations':repetition(relations),
        'evidence':repetition(evidence),
        'field_occurrences':dict(field_counts.most_common()),
        'next_actions':data.get('next_actions',[]),
        'has_source_in_text':'## Source context' in text,
        'has_source_markdown_field_in_structured':any('source_markdown' in x for x in records),
    })

audit={
    'source_commit':capture['source_commit'],
    'captured_at':capture['captured_at'],
    'responses_sha256':hashlib.sha256(raw).hexdigest(),
    'html_sha256':hashlib.sha256(page.encode()).hexdigest(),
    'html_payloads_equal_capture':True,
    'method': 'Unicode code points and UTF-8 bytes; compact JSON with ensure_ascii=False. No token or LLM performance measurements.',
    'repetition_note': 'Repeated object sizes are descriptive and overlap between entities, relations and evidence. Do not add them or treat them as achievable net savings; references and lookup overhead are not included.',
    'cases':cases,
    'totals': {k:sum(c[k] for c in cases) for k in ['text_chars','structured_chars','combined_content_chars','call_tool_result_bytes']},
}
(ROOT/'analysis.json').write_text(json.dumps(audit,ensure_ascii=False,indent=2)+'\n')
for c in cases:
    print(f"{c['id']:16} text={c['text_chars']:5} json={c['structured_chars']:6} ratio={c['json_to_text_ratio']:5} bytes={c['call_tool_result_bytes']:6} entity={c['entities']['occurrences']}/{c['entities']['distinct_objects']} relation={c['relations']['occurrences']}/{c['relations']['distinct_objects']}")
print('Totals',audit['totals'])
