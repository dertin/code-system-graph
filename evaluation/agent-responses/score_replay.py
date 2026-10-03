"""Evidence-bound checks of model answers; not a substitute for semantic/human review."""
import argparse
import json
from pathlib import Path


def records(value):
    if isinstance(value, dict):
        yield value
        for child in value.values(): yield from records(child)
    elif isinstance(value, list):
        for child in value: yield from records(child)


def load_json(path):
    text = path.read_text().strip()
    if text.startswith('```'): text = text.split('\n', 1)[1].rsplit('```', 1)[0]
    return json.loads(text)


def score(capture_path, answer_path):
    capture = load_json(capture_path)
    cases = {case['id']: case for case in capture['cases']}
    output = []
    aliases = set(capture['repo_revisions'])
    for answer in load_json(answer_path)['answers']:
        case = cases[answer['id']]
        response = case['response']
        data = response.get('structuredContent')
        if data is None: data = json.loads(response['content'][0]['text'])
        text = json.dumps(response, ensure_ascii=False)
        facts = list(records(data))
        paths = {record[key] for record in facts for key in ('path','file_path') if isinstance(record.get(key),str)}
        expected_actions = [{'tool': action['tool'], 'arguments': action['arguments']} for record in facts for action in record.get('next_actions',[])]
        supplied_aliases = {alias for alias in aliases if alias in text}
        bad_repositories = [alias for alias in answer['repositories'] if alias not in supplied_aliases]
        bad_paths = [path for path in answer['evidence_paths'] if not any(path == valid or path.endswith('/'+valid) or path.endswith(':'+valid) for valid in paths)]
        bad_actions = [action for action in answer['next_actions'] if action not in expected_actions]
        # Error recovery may propose a documented discovery call, rather than a next_actions field.
        if case['id'] == 'bad-scope':
            bad_actions = [action for action in bad_actions if action != {'tool':'status','arguments':{}}]
        missing_actions = bool(expected_actions) and not answer['next_actions']
        mandatory = []
        if case['id'] == 'events': mandatory = ['api','worker']
        if case['id'] in ('http-filtered',): mandatory = ['api','web','tests']
        if case['id'] == 'web-explore' and any(record.get('federated_handoffs') for record in facts): mandatory = ['web','api']
        missing_repositories = sorted(set(mandatory) - set(answer['repositories']))
        result = {'id':case['id'],'unsupported_repositories':bad_repositories,'unsupported_paths':bad_paths,'nonmatching_actions':bad_actions,'missing_suggested_action':missing_actions,'missing_required_repositories':missing_repositories,'claimed_bug':answer['proves_bug']}
        result['checks_passed'] = not any([bad_repositories,bad_paths,bad_actions,missing_actions,missing_repositories,answer['proves_bug']])
        output.append(result)
    return output


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('capture',type=Path)
    parser.add_argument('answers',type=Path)
    args = parser.parse_args()
    rows = score(args.capture,args.answers)
    print(json.dumps({'checked_cases':len(rows),'passed_cases':sum(row['checks_passed'] for row in rows),'details':rows},ensure_ascii=False,indent=2))
