"""Probe evidence must compare equivalent network state across a JSON journal."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('probe_linux', Path(__file__).parents[1] / 'desktop/scripts/bridge/probe-linux.py')
probe = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(probe)


class LinuxProbeEvidence(unittest.TestCase):
    def test_persisted_baseline_compares_equal_to_the_same_network(self):
        outputs = ['profile-uuid', '[{"addr_info":[{"local":"192.0.2.5","prefixlen":24,"family":"inet"}]}]',
                   '[{"dst":"default","gateway":"192.0.2.1"}]']
        with patch.object(probe, 'command', side_effect=outputs):
            state = probe.snapshot('eth0')
        self.assertEqual(state, json.loads(json.dumps(state)))


if __name__ == '__main__':
    unittest.main()
