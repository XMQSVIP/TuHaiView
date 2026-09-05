import csv
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from summarize_perf import analyze
from summarize_runs import collect
from validate_ui_run import validate, validate_result

class ReportTests(unittest.TestCase):
    def test_native_modal_disqualifies_an_automated_performance_run(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'
            samples=[dict(kind='run_header',schema=2,scenario_name='open')]
            for name,value in [('native_dialog_open',1),('native_dialog_wait_ms',126000),
                ('input_frame_wall_ms',126005),('input_frame_processing_ms',5),
                ('soak_completed_seconds',180),('log_flush',1),('log_dropped',0)]:
                samples.append(dict(name=name,value=value,time_ms=0))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            report=analyze(path)
            self.assertIn('native_modal_interrupted_automated_run',report['invalid_reasons'])
            self.assertEqual(report['native_dialog']['count'],1)
            self.assertEqual(report['native_dialog']['wait_ms']['maximum'],126000)
            self.assertEqual(report['metrics']['input_frame_processing_ms']['maximum'],5)

    def test_displayed_time_uses_previous_frame_duration_and_excludes_boundaries(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'; capture=Path(root)/'display.csv'
            samples=[dict(kind='run_header',scenario_name='open')]
            for qpc,phase in [(100,0),(200,1),(400,7)]:
                samples.append(dict(name='frame_interval_ms',value=1,qpc=qpc,scenario=phase,time_ms=0))
            for name in ['soak_completed_seconds','log_flush','log_dropped']:
                samples.append(dict(name=name,value=0,time_ms=2))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            # DisplayedTime belongs to its row's frame, while legacy
            # MsBetweenDisplayChange belongs to the preceding displayed frame.
            # Keep only complete intervals inside phase 1; discard phase edges
            # and skip a frame which never reached the display.
            capture.write_text('CPUStartQPC,DisplayedTime\n110,900\n210,16\n220,NA\n230,20\n410,800\n')
            result=analyze(path,capture)
            self.assertEqual(result['presentmon']['displayed_by_phase']['1']['n'],1)
            self.assertEqual(result['presentmon']['displayed_by_phase']['1']['maximum'],16)
            self.assertEqual(result['presentmon']['excluded_phase_transitions'],2)
            self.assertEqual(result['presentmon']['not_displayed'],1)

    def test_one_run_copied_five_times_is_not_five_repeats(self):
        with tempfile.TemporaryDirectory() as root:
            root=Path(root);log=root/'log.jsonl';capture=root/'display.csv'
            log.write_text('');capture.write_text('')
            metadata=dict(sha256='binary',exit_code=0,presentmon_exit=0,
                logs=[str(log)],presentmon=str(capture),dataset_manifest={'sha256':'data'},
                dwm=dict(hresult='0x0',refresh_n=60,refresh_d=1))
            for n in range(5): (root/f'{n}-run.json').write_text(json.dumps(metadata))
            result=dict(header=dict(run_id=7),invalid_reasons=[],metrics={},scroll_acceptance={'passed':True})
            with patch('summarize_runs.analyze',return_value=result):
                report=collect(root)
            self.assertFalse(report['unique_runs'])
            self.assertFalse(report['valid_five_runs'])
            self.assertFalse(report['scroll_passed_all_five'])

    def test_metadata_cannot_refer_to_another_process_or_scenario(self):
        result=dict(invalid_reasons=[],metrics={},header=dict(pid=100,scenario_name='scroll'))
        errors=validate_result(dict(pid=101,scenario='open'),result)
        self.assertIn('metadata_log_pid_mismatch',errors)
        self.assertIn('metadata_log_scenario_mismatch',errors)

    def test_saved_run_contract_is_used_during_later_aggregation(self):
        result=dict(invalid_reasons=[],metrics={
            'catalog_displayed_records':dict(maximum=27000),
            'soak_completed_seconds':dict(maximum=60),
            'grid_scroll_offset':dict(maximum=0),
        })
        errors=validate_result(dict(scenario='open',expected_records=50000,
            requested_seconds=90,require_scan_completion=True),result)
        self.assertIn('catalog_count_27000_expected_50000',errors)
        self.assertIn('full_scan_did_not_finish',errors)
        self.assertIn('first_screen_did_not_finish',errors)
        self.assertIn('scenario_ended_before_requested_duration',errors)

    def test_short_or_sparse_memory_run_never_passes_full_duration(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/'samples.jsonl'
            samples=[dict(kind='run_header',schema=2,scenario_name='soak')]
            # Stable values with 24 minute bins are insufficient when there is
            # only one sample in each minute or the process stopped before 30m.
            for minute in range(29):
                samples.append(dict(name='process_private_bytes',value=100000,
                    monotonic_us=minute*60000000))
            for name,value in [('soak_completed_seconds',1740),('log_flush',1),('log_dropped',0)]:
                samples.append(dict(name=name,value=value))
            path.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            result=analyze(path)['memory_stability']
            self.assertTrue(result['threshold_passed'])
            self.assertFalse(result['full_30_minutes'])
            self.assertFalse(result['steady_window_sample_coverage'])
            self.assertFalse(result['passed'])

    def test_partial_catalog_cannot_be_used_as_completed_warmup(self):
        with tempfile.TemporaryDirectory() as root:
            root=Path(root); log=root/'samples.jsonl'; capture=root/'display.csv'; metadata=root/'run.json'
            samples=[dict(kind='run_header',schema=2,pid=123,scenario_name='open')]
            for name,value in [('soak_completed_seconds',90),('log_flush',1),('log_dropped',0),('catalog_displayed_records',27414),('first_screen_ms',300),('grid_scroll_offset',500)]:
                samples.append(dict(name=name,value=value,time_ms=0))
            log.write_text(''.join(json.dumps(s)+'\n' for s in samples))
            capture.write_text('ProcessID,CPUStartQPC,MsBetweenDisplayChange\n'+''.join(f'123,{q},16\n' for q in range(40)))
            metadata.write_text(json.dumps(dict(exit_code=0,scenario='open',logs=[str(log)],presentmon=str(capture))))
            errors=validate(metadata,50000,True)
            self.assertIn('catalog_count_27414_expected_50000',errors)
            self.assertIn('full_scan_did_not_finish',errors)
            self.assertIn('open_scenario_scrolled_automatically',errors)

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
