import threading
import time
import unittest


from kakaotalk_outbound import (
    CONFIRMATION_VALUE,
    AXAvailability,
    AXPreparation,
    AXSendOutcome,
    DraftError,
    DraftStore,
    OutboundCoordinator,
    OutboundError,
)


class FakeReadService:
    def __init__(self):
        self.rooms = [{"chatId": 101, "displayName": "Synthetic room"}]
        self.calls = []

    def list_rooms(self, *, limit):
        self.calls.append(("list_rooms", limit))
        return list(self.rooms)


class FakeAXAdapter:
    def __init__(self):
        self.calls = []
        self.availability = AXAvailability.available()
        self.preparation = AXPreparation.ready("synthetic-receipt")
        self.outcome = AXSendOutcome.invoked()

    def check_availability(self):
        self.calls.append(("check_availability",))
        return self.availability

    def prepare_text(self, *, expected_identity, text):
        self.calls.append(("prepare_text", expected_identity, text))
        return self.preparation

    def send_prepared(self, preparation):
        self.calls.append(("send_prepared", preparation.receipt))
        return self.outcome


class OutboundDraftTests(unittest.TestCase):
    def setUp(self):
        self.now = 10.0
        self.service = FakeReadService()
        self.adapter = FakeAXAdapter()
        self.store = DraftStore(ttl_seconds=30, clock=lambda: self.now, token_factory=lambda: "synthetic-draft-token")
        self.coordinator = OutboundCoordinator(
            service=self.service,
            adapter=self.adapter,
            drafts=self.store,
            enabled=True,
        )

    def test_preview_validates_exact_room_identity_and_never_calls_ax(self):
        preview = self.coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
        self.assertEqual(preview.draft_id, "synthetic-draft-token")
        self.assertEqual(preview.target_identity, "Synthetic room")
        self.assertEqual(preview.text, "Synthetic text")
        self.assertEqual(preview.expires_in_seconds, 30)
        self.assertEqual(self.service.calls, [("list_rooms", 100)])
        self.assertEqual(self.adapter.calls, [])

    def test_preview_rejects_identity_and_text_bounds(self):
        cases = (
            {"room_id": 101, "target_identity": "Wrong room", "text": "ok"},
            {"room_id": 0, "target_identity": "Synthetic room", "text": "ok"},
            {"room_id": 101, "target_identity": "Synthetic room", "text": ""},
            {"room_id": 101, "target_identity": "Synthetic room", "text": "x" * 2001},
            {"room_id": 101, "target_identity": "Synthetic room", "text": "unsafe\x1btext"},
        )
        for values in cases:
            with self.subTest(values=values), self.assertRaises(OutboundError):
                self.coordinator.preview(**values)
        self.assertEqual(self.adapter.calls, [])

    def test_preview_refuses_ambiguous_or_incompletely_bounded_room_identity(self):
        self.service.rooms.append({"chatId": 102, "displayName": "Synthetic room"})
        with self.assertRaises(OutboundError) as raised:
            self.coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
        self.assertEqual(raised.exception.code, "target_invalid")
        self.service.rooms = [
            {"chatId": index + 1, "displayName": f"Synthetic room {index + 1}"}
            for index in range(100)
        ]
        with self.assertRaises(OutboundError):
            self.coordinator.preview(room_id=1, target_identity="Synthetic room 1", text="Synthetic text")
        self.assertEqual(self.adapter.calls, [])

    def test_draft_store_has_a_bounded_capacity(self):
        tokens = iter(("synthetic-first-draft", "synthetic-second-draft"))
        store = DraftStore(max_active_drafts=1, token_factory=lambda: next(tokens))
        store.create(room_id=101, target_identity="Synthetic room", text="First")
        with self.assertRaises(DraftError) as raised:
            store.create(room_id=101, target_identity="Synthetic room", text="Second")
        self.assertEqual(raised.exception.code, "unavailable")

    def test_confirmation_requires_exact_value_is_one_time_and_expires(self):
        self.coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
        with self.assertRaises(OutboundError):
            self.coordinator.confirm(draft_id="synthetic-draft-token", confirmation="send")
        self.assertEqual(self.adapter.calls, [])

        result = self.coordinator.confirm(draft_id="synthetic-draft-token", confirmation=CONFIRMATION_VALUE)
        self.assertEqual(result.status, "uncertain")
        self.assertEqual(self.adapter.calls[-1], ("send_prepared", "synthetic-receipt"))
        with self.assertRaises(DraftError):
            self.coordinator.confirm(draft_id="synthetic-draft-token", confirmation=CONFIRMATION_VALUE)

        expiring = DraftStore(ttl_seconds=30, clock=lambda: self.now, token_factory=lambda: "expiring-draft-token")
        coordinator = OutboundCoordinator(service=self.service, adapter=self.adapter, drafts=expiring, enabled=True)
        coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
        self.now = 40.0
        with self.assertRaises(DraftError):
            coordinator.confirm(draft_id="expiring-draft-token", confirmation=CONFIRMATION_VALUE)

    def test_confirmation_refuses_disabled_unavailable_and_target_mismatch(self):
        cases = (
            (False, AXAvailability.available(), AXPreparation.ready("receipt"), "disabled"),
            (True, AXAvailability.unavailable(), AXPreparation.ready("receipt"), "unavailable"),
            (True, AXAvailability.available(), AXPreparation.mismatch(), "mismatch"),
        )
        for enabled, availability, preparation, expected in cases:
            with self.subTest(expected=expected):
                adapter = FakeAXAdapter()
                adapter.availability = availability
                adapter.preparation = preparation
                store = DraftStore(token_factory=lambda: "synthetic-" + expected + "-draft")
                coordinator = OutboundCoordinator(
                    service=self.service,
                    adapter=adapter,
                    drafts=store,
                    enabled=enabled,
                )
                coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
                with self.assertRaises(OutboundError) as raised:
                    coordinator.confirm(draft_id="synthetic-" + expected + "-draft", confirmation=CONFIRMATION_VALUE)
                self.assertEqual(raised.exception.code, expected)
                self.assertNotIn("send_prepared", [call[0] for call in adapter.calls])

    def test_readback_mismatch_never_reaches_explicit_send(self):
        self.adapter.preparation = AXPreparation.readback_mismatch()
        self.coordinator.preview(room_id=101, target_identity="Synthetic room", text="Synthetic text")
        with self.assertRaises(OutboundError) as raised:
            self.coordinator.confirm(draft_id="synthetic-draft-token", confirmation=CONFIRMATION_VALUE)
        self.assertEqual(raised.exception.code, "mismatch")
        self.assertEqual([call[0] for call in self.adapter.calls], ["check_availability", "prepare_text"])

    def test_concurrent_confirmations_serialize_prepare_through_send(self):
        class OverlapDetectingAdapter(FakeAXAdapter):
            def __init__(self):
                super().__init__()
                self.inflight = 0
                self.overlapped = False
                self.lock = threading.Lock()

            def prepare_text(self, *, expected_identity, text):
                with self.lock:
                    self.inflight += 1
                    if self.inflight > 1:
                        self.overlapped = True
                time.sleep(0.05)
                return AXPreparation.ready(text)

            def send_prepared(self, preparation):
                with self.lock:
                    self.inflight -= 1
                return AXSendOutcome.invoked()

        adapter = OverlapDetectingAdapter()
        tokens = iter(("synthetic-first-draft", "synthetic-second-draft"))
        coordinator = OutboundCoordinator(
            service=self.service,
            adapter=adapter,
            drafts=DraftStore(token_factory=lambda: next(tokens)),
            enabled=True,
        )
        drafts = [
            coordinator.preview(room_id=101, target_identity="Synthetic room", text=text).draft_id
            for text in ("First text", "Second text")
        ]
        start = threading.Barrier(3)
        errors = []

        def confirm(draft_id):
            start.wait()
            try:
                coordinator.confirm(draft_id=draft_id, confirmation=CONFIRMATION_VALUE)
            except Exception as exc:
                errors.append(exc)

        threads = [threading.Thread(target=confirm, args=(draft_id,)) for draft_id in drafts]
        for thread in threads:
            thread.start()
        start.wait()
        for thread in threads:
            thread.join()
        self.assertEqual(errors, [])
        self.assertFalse(adapter.overlapped)


if __name__ == "__main__":
    unittest.main()
