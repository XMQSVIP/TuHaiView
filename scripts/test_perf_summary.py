import csv
import json
import tempfile
import unittest
from pathlib import Path
from summarize_perf import analyze

class ReportTests(unittest.TestCase):
    def test_incomplete_run_never_passes(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/'samples.jsonl'
            path.write_text(json.dumps(dict(name='frame_interval_ms',value=1,time_ms=0))+'\n')
            report=analyze(path)
            self.assertFalse(report['log_valid'])
            self.assertIn('missing_scenario_completion',report['invalid_reasons'])

    def test_phase_is_attached_to_frame_not_previous_log_event(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'
            samples=[dict(kind='run_header',scenario_name='open'),dict(name='trajectory_phase',value=3,time_ms=0),dict(name='frame_interval_ms',value=17,time_ms=1,scenario=1,qpc=100)]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            report=analyze(path)
            self.assertEqual(report['frame_intervals_by_phase']['1']['median'],17)
            self.assertNotIn('3',report['frame_intervals_by_phase'])

    def test_missing_display_frames_cannot_be_reported_as_pass(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'; capture=Path(root)/'display.csv'
            samples=[dict(kind='run_header',scenario_name='scroll')]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            capture.write_text('CPUStartQPC,MsBetweenDisplayChange\n100,NA\n')
            report=analyze(path,capture,59)
            self.assertFalse(report['scroll_acceptance']['passed'])

if __name__ == '__main__': unittest.main()
