"""Verify saved scoring/provenance without model requests: uv run python scripts/verify_rescore.py ORIGINAL REVISED."""
import json
import math
import sys
from collections import defaultdict
from pathlib import Path


def close(a, b):
    assert math.isclose(a, b, abs_tol=1e-12), (a, b)


def preserved(a, b):
    if isinstance(a, (int, float)) and not isinstance(a, bool):
        close(a, b)
    elif isinstance(a, dict):
        assert a.keys() == b.keys()
        for key in a:
            preserved(a[key], b[key])
    elif isinstance(a, list):
        assert len(a) == len(b)
        for x, y in zip(a, b):
            preserved(x, y)
    else:
        assert a == b, (a, b)


def verify(original, revised):
    old = {(c['case'], c['provider']): c for c in original['cases']}
    assert old.keys() == {(c['case'], c['provider']) for c in revised['cases']}
    assert revised['run']['scoring'] == 6
    weights = revised['run']['weights']
    close(sum(weights.values()), 1.0)
    models = defaultdict(list)
    plans = {}
    rounds = 0
    severe = 0
    failed = 0
    for c in revised['cases']:
        source = old[c['case'], c['provider']]
        for key in ['input', 'expected', 'model']:
            assert c[key] == source[key]
        assert len(c['rounds']) == len(source['rounds'])
        plans.setdefault(c['case'], c['correction_tasks'])
        assert plans[c['case']] == c['correction_tasks']
        metrics = []
        for r, before in zip(c['rounds'], source['rounds']):
            rounds += 1
            for key in ['output', 'duration_ms', 'tokens', 'error']:
                assert r[key] == before[key]
            preserved(before['base_score'], r['base_score'])
            s = r['balanced_score']
            assert not s['evaluation_errors'], s['evaluation_errors']
            base = s['base']
            close(base['cleanup'], .5*base['tidy'] + .25*base['readability'] + .25*base['style'])
            for task, plan in zip(s['tasks'], c['correction_tasks']):
                assert task['id'] == plan['id']
                assert task['occurrences'] == len(plan['site_indices'])
                credits = [((r['base_score']['sites'][i]['credit'] or 0.0) if r['base_score'] else 0.0) for i in plan['site_indices']]
                preserved(credits, task['occurrence_credits'])
                close(task['credit'], sum(credits)/len(credits))
            close(base['correction'], sum(t['credit'] for t in s['tasks'])/len(s['tasks']) if s['tasks'] else 1.0)
            final = s['final_metrics']
            assert all(0 <= v <= 1 for v in final.values())
            verdict = r['fidelity']
            assert verdict['credit'] in [0.0, 1.0]
            assert not verdict['evaluation_error']
            if r['error']:
                failed += 1
                assert all(v == 0 for v in final.values())
            elif verdict['credit'] == 0:
                severe += 1
                assert verdict['secondary']['severe'] is True
                assert all(v == 0 for k, v in final.items() if k != 'latency')
                close(final['latency'], base['latency'])
            else:
                preserved(base, final)
            metrics.append(final)
        for key, value in c['balanced_metrics'].items():
            close(value, sum(m[key] for m in metrics)/len(metrics))
        close(c['case_composite'], 100*sum(weights[k]*c['balanced_metrics'][k] for k in weights))
        models[c['provider']].append(c['balanced_metrics'])
    scores = []
    for r in revised['ranking']:
        assert not r['provisional'] and r['unjudged'] == 0
        cases = models[r['provider']]
        assert r['cases'] == len(cases) == len(plans)
        m = {k: sum(c[k] for c in cases)/len(cases) for k in weights}
        close(r['composite'], 100*sum(weights[k]*m[k] for k in weights))
        close(r['ability'], 100*sum(weights[k]*m[k] for k in weights if k != 'latency')/(1-weights['latency']))
        for name in ['correction', 'fidelity', 'cleanup']:
            close(r[name+'_rate'], m[name])
        if 'assessment_version' in revised['run']:
            assert r['assessment'].strip() and '\n' not in r['assessment']
            e = r['assessment_evidence']
            records = [c for c in revised['cases'] if c['provider'] == r['provider']]
            outputs = [o for c in records for o in c['rounds']]
            successful = [o for o in outputs if not o['error']]
            judged = [o for o in successful if o['fidelity']['credit'] in [0.0, 1.0] and not o['fidelity']['evaluation_error']]
            assert e['calls'] == len(outputs)
            assert e['successful_calls'] == len(successful)
            assert e['failed_calls'] == len(outputs)-len(successful)
            assert e['evaluated_fidelity_outputs'] == len(judged)
            assert e['severe_outputs'] == sum(o['fidelity']['credit'] == 0 for o in judged)
            assert e['evaluator_error_outputs'] == sum(bool(o['balanced_score']['evaluation_errors']) for o in outputs)
            timings = [o['duration_ms'] for o in successful]
            assert e['average_successful_latency_ms'] == (sum(timings)//len(timings) if timings else 0)
            assert e['successful_calls_over_5_seconds'] == sum(t > 5000 for t in timings)
            if e['severe_outputs'] > 0 or not judged:
                assert '未检出严重幻觉' not in r['assessment']
            for miss in e['repair_misses']:
                c = next(c for c in records if c['case'] == miss['case'])
                index, task = next((i, t) for i, t in enumerate(c['correction_tasks']) if t['id'] == miss['task_id'])
                eligible = [o for o in c['rounds'] if not o['error'] and all(o['base_score']['sites'][i]['credit'] is not None for i in task['site_indices'])]
                close(miss['mean_credit'], sum(o['balanced_score']['tasks'][index]['credit'] for o in eligible)/len(eligible))
                assert miss['heard'] == task['heard'] and miss['intended'] == task['intended']
                assert miss['evaluated_rounds'] == len(eligible)
                assert miss['occurrences_per_round'] == len(task['site_indices'])
                close(miss['repair_loss_points'], 100*weights['correction']*(1-miss['mean_credit'])*len(eligible)/len(c['rounds'])/len(c['correction_tasks'])/len(records))
        scores.append(r['composite'])
    assert len(scores) == len(models)
    assert scores == sorted(scores, reverse=True)
    print(f'PASS: {len(models)} models, {len(plans)} equally weighted cases, {rounds} preserved outputs/timings/base scores; {severe} confirmed severe, {failed} failed calls, no pending/evaluator errors.')


if __name__ == '__main__':
    verify(*(json.loads(Path(p).read_text()) for p in sys.argv[1:]))
