"""The chart's terminationGracePeriodSeconds floor covers the whole shutdown.

#2046 W1.10: the server bounds its shutdown by one enclosing deadline
(`crates/waddle-server/src/shutdown_deadline.rs`). The chart floor in
`templates/validations.yaml` must be preStop + every term of that deadline +
the kill margin, and the two fixed terms must match the Rust constants.
"""

import json
from pathlib import Path
import re
import subprocess
import unittest

SERVER = Path(__file__).resolve().parents[1]
CHART = SERVER / 'charts/waddle-server'
VALIDATIONS = CHART / 'templates/validations.yaml'
DEADLINE_RS = SERVER / 'crates/waddle-server/src/shutdown_deadline.rs'
SECRET_ARGS = [
    '--set-string', 'secret.sessionKey=ci-session-key',
    '--set-string', 'secret.occupantIdSecret=ci-occupant-id-secret-32-bytes-long',
    '--set', 'spicedb.enabled=false',
]


def chart_constant(name):
    match = re.search(r'\{\{-\s*\$' + name + r'\s*:=\s*(\d+)\s*-\}\}', VALIDATIONS.read_text())
    if match is None:
        raise AssertionError(f'${name} not found in {VALIDATIONS}')
    return int(match.group(1))


def rust_seconds(name):
    match = re.search(
        r'pub const ' + name + r': Duration = Duration::from_secs\((\d+)\);',
        DEADLINE_RS.read_text(),
    )
    if match is None:
        raise AssertionError(f'{name} not found in {DEADLINE_RS}')
    return int(match.group(1))


def render(*overrides):
    args = ['helm', 'template', 'waddle-server', str(CHART), '--namespace', 'waddle', *SECRET_ARGS]
    for override in overrides:
        args += [override] if override.startswith('--') else ['--set', override]
    return subprocess.run(args, text=True, capture_output=True)


def yaml_to_json(text):
    result = subprocess.run(['yq', '-o=json', '.'], input=text, text=True, capture_output=True, check=True)
    return json.loads(result.stdout)


def defaults():
    return yaml_to_json((CHART / 'values.yaml').read_text())


class ChartShutdownBudgetTests(unittest.TestCase):
    def test_fixed_stage_terms_match_the_server_constants(self):
        self.assertEqual(chart_constant('retireBudget'), rust_seconds('SHUTDOWN_RETIRE_BUDGET'))
        self.assertEqual(
            chart_constant('terminalSweepBudget'),
            rust_seconds('SHUTDOWN_TERMINAL_SWEEP_BUDGET'),
        )

    def floor(self, values):
        return (
            int(values['preStopSleepSeconds'])
            + int(values['config']['drainTimeoutSeconds'])
            + int(values['clustering']['claimReleaseBudgetSeconds'])
            + chart_constant('retireBudget')
            + chart_constant('terminalSweepBudget')
            + chart_constant('killMargin')
        )

    def test_default_grace_period_is_exactly_the_floor(self):
        values = defaults()
        self.assertEqual(values['terminationGracePeriodSeconds'], self.floor(values))
        result = render('--show-only=templates/deployment.yaml')
        self.assertEqual(result.returncode, 0, result.stderr)
        deployment = yaml_to_json(result.stdout)
        self.assertEqual(deployment['kind'], 'Deployment')
        self.assertEqual(
            deployment['spec']['template']['spec']['terminationGracePeriodSeconds'],
            values['terminationGracePeriodSeconds'],
        )

    def test_one_second_below_the_floor_fails_naming_every_stage(self):
        values = defaults()
        result = render(f'terminationGracePeriodSeconds={self.floor(values) - 1}')
        self.assertNotEqual(result.returncode, 0)
        for term in (
            'preStopSleepSeconds',
            'config.drainTimeoutSeconds',
            'clustering.claimReleaseBudgetSeconds',
            'identity retirement',
            'terminal sweep',
            'kill margin',
        ):
            self.assertIn(term, result.stderr)

    def test_every_configurable_stage_raises_the_floor(self):
        values = defaults()
        grace = values['terminationGracePeriodSeconds']
        for override in (
            f'preStopSleepSeconds={values["preStopSleepSeconds"] + 1}',
            f'config.drainTimeoutSeconds={values["config"]["drainTimeoutSeconds"] + 1}',
            f'clustering.claimReleaseBudgetSeconds={values["clustering"]["claimReleaseBudgetSeconds"] + 1}',
        ):
            with self.subTest(override=override):
                self.assertNotEqual(render(override).returncode, 0)
                result = render(override, f'terminationGracePeriodSeconds={grace + 1}')
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_old_floor_no_longer_covers_retirement_and_sweep(self):
        # The pre-W1.10 floor (preStop + drain + claimRelease + kill margin)
        # omitted retirement and the terminal sweep.
        values = defaults()
        old_floor = (
            int(values['preStopSleepSeconds'])
            + int(values['config']['drainTimeoutSeconds'])
            + int(values['clustering']['claimReleaseBudgetSeconds'])
            + chart_constant('killMargin')
        )
        self.assertNotEqual(render(f'terminationGracePeriodSeconds={old_floor}').returncode, 0)


if __name__ == '__main__':
    unittest.main()
