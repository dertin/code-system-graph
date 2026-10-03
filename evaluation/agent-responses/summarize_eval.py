"""Summarize recorded usage and evidence checks without treating repetitions as tasks."""
import hashlib
import json
from pathlib import Path
import random
import statistics

from score_replay import load_json, score

ROOT = Path(__file__).resolve().parent
BASELINE = ROOT.parents[1] / 'artifacts/agent-response-examples/responses.json'


def percentile(values, fraction):
    values = sorted(values)
    position = (len(values)-1)*fraction
    lower = int(position)
    upper = min(lower+1,len(values)-1)
    return round(values[lower]+(values[upper]-values[lower])*(position-lower),2)


def summarize():
    runs = []
    for experiment in ['smoke','compact','pilot']:
        directory = ROOT / f'results-llm-{experiment}'
        for path in sorted(directory.glob('*.metadata.json')):
            run = load_json(path)
            capture = BASELINE if run['arm']=='baseline' else ROOT/'results-json/responses.json'
            answer_path = path.with_name(path.name.replace('.metadata.json','.answer.json'))
            checks = score(capture,answer_path)
            run.update(experiment=experiment,answer_sha256=hashlib.sha256(answer_path.read_bytes()).hexdigest(),checks=checks)
            runs.append(run)
    groups = []
    for experiment,arm in sorted({(r['experiment'],r['arm']) for r in runs}):
        group = [r for r in runs if (r['experiment'],r['arm'])==(experiment,arm)]
        total_tokens = [r['usage']['input_tokens']+r['usage']['output_tokens'] for r in group]
        groups.append({'experiment':experiment,'arm':arm,'repetitions':len(group),'cases_per_batch':group[0]['cases'],'mean_total_tokens':round(statistics.mean(total_tokens),1),'latency_p50_seconds':percentile([r['elapsed_seconds'] for r in group],.5),'latency_p95_seconds':percentile([r['elapsed_seconds'] for r in group],.95),'evidence_checks_passed':sum(c['checks_passed'] for r in group for c in r['checks']),'case_observations':sum(len(r['checks']) for r in group)})
    # Only current compact/pilot captures are valid rubric inputs for the later corpus.
    # Earlier smoke content differs; retain usage, do not compare its answer-check scores.
    for group in groups:
        if group['experiment']=='smoke': group['evidence_checks_passed']=None
    paired = {}
    pilot = [r for r in runs if r['experiment']=='pilot']
    for run in pilot:
        for item in run['checks']:
            paired.setdefault(item['id'],{}).setdefault(run['arm'],[]).append(int(item['checks_passed']))
    deltas = [statistics.mean(arms['markdown'])-statistics.mean(arms['json']) for arms in paired.values() if set(arms)=={'markdown','json'}]
    randomizer = random.Random(121)
    interval = None
    if deltas:
        bootstrap = [statistics.mean(randomizer.choices(deltas,k=len(deltas))) for _ in range(10000)]
        interval = {'metric':'automated evidence-check rate only, not task success','unique_cases':len(deltas),'markdown_minus_json':statistics.mean(deltas),'cluster_bootstrap_95_percent':[percentile(bootstrap,.025),percentile(bootstrap,.975)],'seed':121}
    result = {'model_alias':'gpt-6-astra','inference':'remote','model_snapshot_temperature_tokenizer':'not exposed/pinned by this CLI; API-reported usage is retained','method':'batch response replay; no tool execution; not a Themis adapter trace','groups':groups,'paired_evidence_checks':interval,'runs':runs}
    (ROOT/'evaluation-results.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(groups,indent=2))


if __name__ == '__main__': summarize()
