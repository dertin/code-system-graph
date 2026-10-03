"""Validate actual MCP capture parity, references, source, evidence and continuations."""
import argparse
import json
import re
from pathlib import Path


def literal(text):
    match = re.fullmatch(r'(`+) (.*) \1', text)
    assert match, text
    return match[2]


def markdown_facts(text):
    lines = text.splitlines()
    result, stack = {}, []
    i = 4  # title, blank, trusted data boundary, blank
    while i < len(lines):
        line = lines[i]
        depth = (len(line) - len(line.lstrip(' '))) // 2
        text = line[depth * 2:]
        if text.startswith('- '):
            stack = stack[:depth]
            item = re.fullmatch(r'- Item (\d+):', text)
            if item: stack.append(int(item[1]) - 1)
            else:
                marker = re.match(r'- (`+) (.*?) \1: ?(.*)', text)
                assert marker, text
                stack.append(marker[2])
                if marker[3]: result[tuple(stack)] = literal(marker[3])
        elif text.startswith('```') and text.endswith('text'):
            fence, source = text[:-4], []
            i += 1
            while lines[i][depth * 2:] != fence:
                source.append(lines[i][depth * 2:])
                i += 1
            result[tuple(stack[:depth])] = '\n'.join(source)
        elif text:
            result[tuple(stack[:depth])] = literal(text)
        i += 1
    return result


def json_facts(value, path=()):
    if isinstance(value, dict) and value:
        return {key: fact for name, child in value.items() for key, fact in json_facts(child, path + (name,)).items()}
    if isinstance(value, list) and value:
        return {key: fact for index, child in enumerate(value) for key, fact in json_facts(child, path + (index,)).items()}
    return {path: value.rstrip('\n') if isinstance(value, str) else json.dumps(value, separators=(',', ':'))}


def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values(): yield from walk(child)
    elif isinstance(value, list):
        for child in value: yield from walk(child)


def check(json_dir, markdown_dir, budget):
    j = json.loads((json_dir / 'responses.json').read_text())
    m = json.loads((markdown_dir / 'responses.json').read_text())
    assert j['repo_revisions'] == m['repo_revisions']
    markdown = {case['id']:case for case in m['cases']}
    assert len(j['cases']) >= 40
    for case in j['cases']:
        other = markdown[case['id']]
        response, md_response = case['response'], other['response']
        for result in [response, md_response]:
            assert 'structuredContent' not in result, case['id']
            assert len(result['content']) == 1
            assert len(json.dumps(result, ensure_ascii=False, separators=(',', ':')).encode()) <= budget
        value = json.loads(response['content'][0]['text'])
        assert value['schema_version'] == 6
        facts = markdown_facts(md_response['content'][0]['text'])
        expected = json_facts(value)
        # Provider execution counters measure separate calls, not semantic facts.
        ignored = {'provider_operations','maximum_concurrency_observed','retained_bytes'}
        facts = {key:fact for key,fact in facts.items() if key[-1] not in ignored}
        expected = {key:fact for key,fact in expected.items() if key[-1] not in ignored}
        assert facts == expected, (case['id'], list(set(facts.items()) ^ set(expected.items()))[:6])
        fixture = Path(__file__).resolve().parent / '.work' / 'commerce'
        directories = {'api':'api','web':'web','tests':'tests-python','worker':'worker-python','infra':'infra','docs':'docs'}
        for record in walk(value):
            alias, path = record.get('repository_alias'), record.get('path')
            if alias in directories and path:
                source = fixture / directories[alias] / path
                assert source.is_file(), (case['id'], alias, path)
                lines = len(source.read_text().splitlines())
                start, end = record.get('start_line'), record.get('end_line')
                if start is not None: assert 1 <= start <= lines, (case['id'], path, start, lines)
                if end is not None: assert (start or 1) <= end <= lines, (case['id'], path, end, lines)
            if 'entity_ref' in record: assert record['entity_ref'] in value['entities']
            if 'relation_ref' in record: assert record['relation_ref'] in value['relations']
        assert bool(response.get('isError')) == (case['id'] == 'bad-scope'), case['id']
        if case['id'] == 'web-explore':
            assert value['result']['data']['federated_handoffs'], 'missing web handoff'
            assert value['result']['data']['federated_handoffs'][0]['remote_repository']['alias'] == 'api'
        if case['id'] == 'test-explore': assert '201' in value['result']['data']['source_markdown']
        if case['id'] == 'test-query':
            assert any(e['label'] == 'python/pytest::test_create_order' and e['path'] == 'tests/test_orders.py' for e in value['entities'].values())
        if case['id'] == 'events':
            evidence = [e for relation in value['relations'].values() if relation.get('derivation') == 'event_delivery_path' for e in relation['evidence']]
            assert any(e['repository_alias'] == 'worker' and e['path'] == 'worker.py' and e['start_line'] == 5 for e in evidence)
            assert any(e['repository_alias'] == 'api' for e in evidence)
        if case['id'] == 'missing':
            assert value['result']['status'] == 'ok'
            assert not any('Full-text scores' in gap for gap in value['result']['data']['coverage']['gaps'])
    continuations = json.loads((json_dir / 'continuations.json').read_text())
    assert continuations
    for continuation in continuations:
        result = continuation['result']
        assert not result.get('isError') and 'protocol_error' not in result, continuation
    print(json.dumps({'cases': len(j['cases']), 'semantic_parity': 'pass', 'live_continuations': len(continuations), 'references': 'pass', 'budget_bytes': budget}, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('json_dir', type=Path)
    parser.add_argument('markdown_dir', type=Path)
    parser.add_argument('--budget', type=int, default=65536)
    args = parser.parse_args()
    check(args.json_dir, args.markdown_dir, args.budget)
