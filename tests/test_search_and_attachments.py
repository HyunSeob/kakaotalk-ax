import base64
import json
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from kakaotalk_automation import ax_row
from test_ax_reader import SnapshotNative
from test_ax_operations import FixtureTestCase


class SearchNavigationTests(FixtureTestCase):
    def test_visits_older_results_and_keeps_context(self):
        def control(action, **args):
            if action == 'open':
                return {'status': 'ok'}
            self.native.rows['Fixture'] = ['context', 'dress latest'] if action == 'search_query' else ['dress older', 'tail']
            return {'status': 'ok', 'searchVerified': True,
                    'searchState': {'previousEnabled': action == 'search_query'}}
        with patch.object(self.native, '_control', side_effect=control):
            result = self.native.search_messages('Fixture', 'dress')
        self.assertEqual([x['text'] for x in result['items']], ['dress latest', 'dress older'])
        self.assertEqual(result['items'][0]['contextBefore'][0]['text'], 'context')
        self.assertEqual(result['stopReason'], 'navigation_exhausted')
        self.assertTrue(result['duplicatesPossible'])
        self.assertFalse(result['coverage']['historyComplete'])

    def test_attachment_context_is_preserved_without_searching_it_as_text(self):
        original = self.native.read_messages
        def read(*args, **kwargs):
            page = original(*args, **kwargs)
            page['items'][0].update(kind='attachment', text=None, attachments=[{'type': 'image'}])
            return page
        with patch.object(self.native, 'read_messages', side_effect=read):
            result = self.native.search_messages('Fixture', 'repeat', use_app_search=False)
        context = result['items'][0]['contextBefore'][0]
        self.assertEqual(context['kind'], 'attachment')
        self.assertIsNone(context['text'])

    def test_unverified_search_preserves_local_matches_but_is_partial(self):
        original = self.native._control
        def control(action, **args):
            if action == 'search_query':
                return {'status': 'partial', 'searchVerified': False, 'reason': 'search_result_not_observed'}
            return original(action, **args)
        with patch.object(self.native, '_control', side_effect=control):
            result = self.native.search_messages('Fixture', 'repeat')
        self.assertEqual(result['status'], 'partial')
        self.assertFalse(result['searchVerified'])
        self.assertEqual(len(result['items']), 1)
        self.assertEqual(len(result['searchVisits']), 1)

    def test_legacy_ok_without_evidence_is_not_verified(self):
        with patch.object(self.native, '_control', return_value={'status': 'ok'}):
            result = self.native.search_messages('Fixture', 'repeat')
        self.assertEqual(result['status'], 'partial')
        self.assertFalse(result['searchVerified'])

    def test_failure_after_first_result_preserves_prior_items(self):
        original = self.native._control
        def control(action, **args):
            if action == 'search_query':
                return {'status': 'ok', 'searchVerified': True, 'searchState': {'previousEnabled': True}}
            if action == 'search_previous':
                return {'status': 'blocked', 'reason': 'search_query_changed'}
            return original(action, **args)
        with patch.object(self.native, '_control', side_effect=control):
            result = self.native.search_messages('Fixture', 'repeat')
        self.assertEqual(result['status'], 'partial')
        self.assertEqual(result['items'][0]['text'], 'repeat')
        self.assertEqual(result['stopReason'], 'search_query_changed')

    def test_step_budget_and_repeated_body_are_not_silently_deduplicated(self):
        self.native.rows['Fixture'] = ['dress', 'dress']
        original = self.native._control
        def control(action, **args):
            if action in ('search_query', 'search_previous'):
                return {'status': 'ok', 'searchVerified': True, 'searchState': {'previousEnabled': True}}
            return original(action, **args)
        with patch.object(self.native, '_control', side_effect=control):
            result = self.native.search_messages('Fixture', 'dress', max_steps=2)
        self.assertEqual(result['total'], 2)
        self.assertEqual(len(result['searchVisits']), 2)
        self.assertEqual(result['stopReason'], 'step_limit')

    def test_invalid_new_arguments_do_not_navigate(self):
        for options in ({'max_steps': 0}, {'max_steps': 51}, {'context_rows': -1}, {'max_steps': True}):
            with self.assertRaises(ValueError):
                self.native.search_messages('Fixture', 'dress', **options)
        self.assertEqual(self.native.controls, [])


class AttachmentTests(unittest.TestCase):
    def setUp(self):
        self.native = SnapshotNative()
        self.snapshot = self.native.read_messages('Fixture')['snapshotId']

    def test_unnamed_image_is_preserved_without_making_up_body(self):
        result = ax_row({'rowIndex': 7, 'parts': [{'role': 'AXImage'}]})
        self.assertEqual(result['kind'], 'attachment')
        self.assertIsNone(result['text'])
        self.assertIsNone(result['attachments'][0]['description'])

    def test_decorative_image_does_not_reclassify_text_message(self):
        result = ax_row({'rowIndex': 7, 'parts': [{'role': 'AXImage'}, {'role': 'AXTextArea', 'AXValue': 'hello'}]})
        self.assertEqual(result['kind'], 'text')
        self.assertEqual(result['attachments'], [])

    def test_snapshot_change_and_non_image_never_capture(self):
        with patch('kakaotalk_automation.subprocess.run') as run:
            result = self.native.read_attachment('Fixture', 4, '0' * 24)
            self.assertEqual(result['status'], 'snapshot_changed')
            result = self.native.read_attachment('Fixture', 0, self.snapshot)
            self.assertEqual(result['reason'], 'attachment_row_required')
            run.assert_not_called()

    def test_image_is_private_and_ocr_is_opt_in(self):
        response = {'status': 'ok', 'ocrPerformed': False,
                    'imageBase64': base64.b64encode(b'fixture image').decode()}
        with tempfile.TemporaryDirectory() as directory, patch.object(self.native, 'build', return_value=Path('/fixture/native')), \
             patch('kakaotalk_automation.subprocess.run', return_value=SimpleNamespace(returncode=0, stdout=json.dumps(response))) as run:
            path = str(Path(directory) / 'crop.png')
            result = self.native.read_attachment('Fixture', 4, self.snapshot, path)
            self.assertFalse(result['ocrPerformed'])
            self.assertNotIn('text', result)
            self.assertNotIn('imageBase64', result)
            if sys.platform != "win32":
                self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o600)
            self.assertEqual(Path(path).read_bytes(), b'fixture image')
            request = json.loads(run.call_args.kwargs['input'])
            self.assertFalse(request['ocr'])
            self.assertEqual(request['expectedCount'], 5)
            with self.assertRaises(ValueError):
                self.native.read_attachment('Fixture', 4, self.snapshot, path)
            run.reset_mock()
            result = self.native.read_attachment('Fixture', 4, self.snapshot, ocr=True)
            self.assertTrue(json.loads(run.call_args.kwargs['input'])['ocr'])
            self.assertTrue(Path(result['imagePath']).is_file())
            Path(result['imagePath']).unlink()
            self.assertNotIn('imageBase64', result)

    def test_invalid_snapshot_and_relative_export_are_rejected(self):
        for kwargs in ({'snapshot_id': ''}, {'snapshot_id': self.snapshot, 'output_path': 'relative.png'}):
            with self.assertRaises(ValueError):
                self.native.read_attachment('Fixture', 4, **kwargs)


if __name__ == '__main__':
    unittest.main()
