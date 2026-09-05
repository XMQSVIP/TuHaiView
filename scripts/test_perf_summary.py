import csv
import json
import tempfile
import unittest
from pathlib import Path
from summarize_perf import analyze

class ReportTests(unittest.TestCase):
    def test_flush_marker_without_durable_certificate_is_invalid(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'
            samples=[dict(kind='run_header',schema=3,run_id=7,scenario_name='open')]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            self.assertIn('missing_flush_certificate',analyze(path)['invalid_reasons'])
            path.with_suffix('.complete.json').write_text(json.dumps(dict(run_id=7,sync_completed=True,bytes=path.stat().st_size)))
            self.assertTrue(analyze(path)['log_valid'])
            with path.open('a') as out: out.write('{}\n')
            self.assertIn('invalid_flush_certificate',analyze(path)['invalid_reasons'])

    def test_display_intervals_crossing_phase_are_excluded(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'; capture=Path(root)/'display.csv'
            samples=[dict(kind='run_header',scenario_name='open')]
            for qpc,phase in [(100,0),(200,1)]:
                samples.append(dict(name='frame_interval_ms',value=1,qpc=qpc,scenario=phase,time_ms=0))
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            capture.write_text('CPUStartQPC,MsBetweenDisplayChange\n110,16\n210,99\n220,16\n')
            result=analyze(path,capture)
            self.assertEqual(result['presentmon']['displayed_by_phase']['1']['maximum'],16)
            self.assertEqual(result['presentmon']['excluded_phase_transitions'],1)
            self.assertIsNone(result['scroll_acceptance']['passed'])

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
