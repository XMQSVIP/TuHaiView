import csv
import json
import tempfile
import unittest
from pathlib import Path
from summarize_perf import analyze
from summarize_runs import collect

class ReportTests(unittest.TestCase):
    def test_open_capture_requires_displayed_frames_from_target_process(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'; capture=Path(root)/'display.csv'
            samples=[dict(kind='run_header',schema=2,pid=123,scenario_name='open')]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            capture.write_text('ProcessID,CPUStartQPC,MsBetweenDisplayChange\n'+''.join(f'999,{q},16\n' for q in range(40)))
            self.assertIn('insufficient_target_display_samples',analyze(path,capture)['invalid_reasons'])
            capture.write_text('ProcessID,CPUStartQPC,MsBetweenDisplayChange\n'+''.join(f'123,{q},16\n' for q in range(40)))
            self.assertTrue(analyze(path,capture)['log_valid'])

    def test_five_successful_process_exits_without_capture_do_not_pass(self):
        with tempfile.TemporaryDirectory() as root:
            directory=Path(root)
            log=directory/'samples.jsonl'
            samples=[dict(kind='run_header',schema=2,scenario_name='open')]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            log.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            for i in range(5):
                run=dict(sha256='same',exit_code=0,presentmon_exit=0,logs=[str(log)],presentmon=str(directory/'missing.csv'),dataset_manifest={'sha256':'fixture'},dwm=dict(hresult='0x0',refresh_n=60,refresh_d=1))
                (directory/f'{i}-run.json').write_text(json.dumps(run))
            result=collect(directory)
            self.assertFalse(result['valid_five_runs'])
            self.assertTrue(all('missing_presentmon_csv' in r['errors'] for r in result['runs']))

    def test_flush_marker_without_durable_certificate_is_invalid(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'
            samples=[dict(kind='run_header',schema=3,run_id=7,scenario_name='open')]
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            for name in ['window_minimized','window_width','window_height','pixels_per_point']:
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
