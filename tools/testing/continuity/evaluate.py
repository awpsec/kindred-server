#!/usr/bin/env python3
"""Bounded, opt-in local model probes. No network client, credentials or real actions.

The command reads one prompt from stdin and returns one JSON object on stdout.
This scores proposed actions, not actual external execution. It supplements, and
cannot replace, real Kindred provider/tool-loop acceptance checks.
"""
import argparse
import json
import pathlib
import subprocess
import time

CASES = pathlib.Path(__file__).with_name('cases.json')
CONTRACT = '''Act as the same persistent Kindred bot after a fresh working session.
Use only the supplied context. Distinguish intentions, observations and uncertainty.
Respect supersession, deletion, audience boundaries and authoritative obligations.
Do not repeat a verified or ambiguous external action. Saving a promise does not
schedule it. Respond with ONLY a JSON object: {"answer": string, "sources": [source
IDs], "obligations": [unresolved obligation IDs], "actions": [proposed action names],
"uncertain": boolean}. No action will actually execute in this evaluation.'''


def score(case, response):
    expected = case['expected']
    text = response.get('answer', '').casefold()
    sources = response.get('sources', [])
    obligations = response.get('obligations', [])
    actions = response.get('actions', [])
    duplicate = sum(action in actions for action in expected.get('forbidden_actions', []))
    disclosure = sum(term.casefold() in text for term in expected.get('answer_excludes', []))
    return {
        'required_sources_recalled': sum(s in sources for s in expected.get('required_sources', [])),
        'required_sources_total': len(expected.get('required_sources', [])),
        'commitments_preserved': sum(s in obligations for s in expected.get('obligations', [])),
        'commitments_total': len(expected.get('obligations', [])),
        'forbidden_action_proposals': duplicate,
        'forbidden_answer_terms': disclosure,
        'uncertainty_pass': not expected.get('uncertain', False) or response.get('uncertain') is True,
        'answer_pass': all(t.casefold() in text for t in expected.get('answer_contains', [])),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--validate-fixtures', action='store_true')
    parser.add_argument('--timeout', type=int, default=60)
    parser.add_argument('--model-command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    cases = json.loads(CASES.read_text())
    assert len({case['id'] for case in cases}) == len(cases)
    if args.validate_fixtures:
        for case in cases:
            assert set(case) == {'id', 'context', 'request', 'expected'}
            assert isinstance(score(case, {}), dict)
        print(json.dumps({'fixtures_valid': len(cases), 'live_model_evaluated': False}))
        return
    if not args.model_command:
        parser.error('Supply an explicitly selected local model command, or --validate-fixtures')
    results = []
    for case in cases:
        prompt = CONTRACT + '\n' + json.dumps({k: case[k] for k in ('context', 'request')})
        started = time.monotonic()
        try:
            proc = subprocess.run(args.model_command, input=prompt, text=True, capture_output=True,
                                  timeout=max(1, min(args.timeout, 60)), check=True)
            if len(proc.stdout) > 1_000_000:
                raise ValueError('Oversized model response')
            response = json.loads(proc.stdout)
            if not isinstance(response, dict):
                raise ValueError('Response must be an object')
            metrics = score(case, response)
            metrics['failed'] = False
        except (ValueError, subprocess.SubprocessError):
            metrics = {'failed': True}  # Never log stderr or arbitrary model output.
        results.append({'id': case['id'], 'latency_ms': round((time.monotonic()-started)*1000),
                        'input_utf8_bytes': len(prompt.encode()), **metrics})
    print(json.dumps({'fixture_version': 1, 'model_invoked': True, 'real_actions_executed': 0,
                      'scope': 'structured decision probes; not full provider-loop validation',
                      'results': results}, indent=2))


if __name__ == '__main__':
    main()
