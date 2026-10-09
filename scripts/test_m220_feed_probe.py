"""Offline checks of the bounded diagnostic; never connect to a printer."""

import io
import time
import unittest
from contextlib import contextmanager, redirect_stdout
from unittest.mock import patch

import m220_feed_probe as probe

READY = bytes.fromhex("1a 07 03 00 01 1a 05 98 1a 03 a8 1a 06 89")


class FakeLink:
    def __init__(
        self,
        *,
        fail_print=False,
        ignore_continuous=False,
        status=READY,
        drop_after_print_failure=False,
        fail_raster_send=False,
    ):
        self.commands = []
        self.reply = b""
        self.mode = probe.GAP_REPLY
        self.fail_print = fail_print
        self.ignore_continuous = ignore_continuous
        self.status = status
        self.drop_after_print_failure = drop_after_print_failure
        self.fail_raster_send = fail_raster_send
        self.dropped = False

    def send(self, command):
        if self.dropped:
            raise OSError("simulated link loss")
        self.commands.append(command)
        if self.fail_raster_send and command.startswith(probe.COPY_ONE):
            raise OSError("simulated partial raster write")
        if command == probe.STATUS:
            self.reply = self.status
        elif command == probe.QUERY_MODE:
            self.reply = self.mode
        elif command == probe.PREAMBLE:
            if not self.ignore_continuous:
                self.mode = probe.CONTINUOUS_REPLY
        elif command == probe.GAP_PREAMBLE:
            self.mode = probe.GAP_REPLY
        elif command == probe.CONTINUOUS:
            if not self.ignore_continuous:
                self.mode = probe.CONTINUOUS_REPLY
        elif command == probe.GAP:
            self.mode = probe.GAP_REPLY
        elif command.startswith(probe.COPY_ONE):
            self.reply = probe.PRINTED

    def receive(self, seconds, *, printed=False):
        if printed and self.fail_print:
            self.dropped = self.drop_after_print_failure
            raise probe.ProbeError("simulated print deadline")
        reply, self.reply = self.reply, b""
        return reply


class FakeSocket:
    """Returns queued chunks, then repeats a chunk or times out."""

    def __init__(self, chunks, *, repeat=None):
        self.chunks = list(chunks)
        self.repeat = repeat

    def settimeout(self, seconds):
        pass

    def recv(self, size):
        if self.chunks:
            return self.chunks.pop(0)
        if self.repeat is not None:
            time.sleep(0.005)
            return self.repeat
        raise TimeoutError("simulated receive timeout")


def connect_sequence(links):
    """A connect() replacement yielding each fake link once, in order."""
    remaining = iter(links)

    @contextmanager
    def fake_connect(address, log):
        yield next(remaining)

    return fake_connect


def run_main(links):
    output = io.StringIO()
    with (
        patch.object(probe, "connect", connect_sequence(links)),
        redirect_stdout(output),
    ):
        code = probe.main(["--address", "XX:XX:XX:XX:XX:XX", "--print"])
    return code, output.getvalue()


def rasters(commands):
    return [command for command in commands if command.startswith(probe.COPY_ONE)]


class ReceiveTests(unittest.TestCase):
    def receive(self, chunks, seconds=1.0, **kwargs):
        link = probe.Link(FakeSocket(chunks, **kwargs), lambda _: None)
        return link.receive(seconds, printed=True)

    def test_completion_split_across_chunks(self):
        self.assertEqual(
            self.receive([b"\x06\x1a", b"\x0f", b"\x0c"]),
            b"\x06\x1a\x0f\x0c",
        )

    def test_failure_report_raises(self):
        with self.assertRaisesRegex(probe.ProbeError, "print failure"):
            self.receive([bytes.fromhex("1a 0f 0b")])

    def test_cancel_report_raises(self):
        with self.assertRaisesRegex(probe.ProbeError, "cancelled"):
            self.receive([bytes.fromhex("1a 0b b8")])

    def test_deadline_expires_on_silence(self):
        with self.assertRaisesRegex(probe.ProbeError, "deadline expired"):
            self.receive([bytes.fromhex("1a 06 89")])

    def test_deadline_is_absolute_despite_unsolicited_status(self):
        start = time.monotonic()
        with self.assertRaisesRegex(probe.ProbeError, "deadline expired"):
            self.receive([], seconds=0.05, repeat=bytes.fromhex("1a 06 89"))
        self.assertLess(time.monotonic() - start, 1.0)

    def test_empty_receive_is_a_disconnect(self):
        with self.assertRaisesRegex(probe.ProbeError, "disconnected"):
            self.receive([b""])


class ProbeTests(unittest.TestCase):
    def test_default_dry_run_cannot_open_connection(self):
        with (
            patch.object(probe, "connect", side_effect=AssertionError("connected")),
            redirect_stdout(io.StringIO()),
        ):
            self.assertEqual(probe.main([]), 0)

    def test_live_run_requires_explicit_address(self):
        with (
            redirect_stdout(io.StringIO()),
            patch("sys.stderr", io.StringIO()),
            self.assertRaises(SystemExit) as error,
        ):
            probe.main(["--print"])
        self.assertEqual(error.exception.code, 2)

    def test_each_marker_has_only_its_own_reference_lane(self):
        for letter, left in zip("ABC", (24, 120, 216)):
            payload = probe.marker(letter, left)
            self.assertEqual(
                payload[:12], bytes.fromhex("1f 11 21 01 1d 76 30 00 28 00 10 00")
            )
            self.assertEqual(len(payload), 652)
            expected = ((1 << 80) - 1) << (320 - left - 80)
            for row in (payload[12:52], payload[52:92]):
                self.assertEqual(int.from_bytes(row, "big"), expected)

    def test_success_sends_three_rasters_and_only_one_explicit_feed(self):
        for feed in (4, 8):
            link, state = FakeLink(), probe.State()
            probe.run_experiment(link, feed, state, lambda _: None, lambda _: None)
            rasters = [
                command
                for command in link.commands
                if command.startswith(probe.COPY_ONE)
            ]
            self.assertEqual(len(rasters), 3)
            self.assertEqual(state.rasters_submitted, 3)
            explicit_feed = bytes((0x1B, 0x64, feed))
            self.assertEqual(link.commands.count(explicit_feed), 1)
            self.assertLess(
                link.commands.index(rasters[1]), link.commands.index(explicit_feed)
            )
            self.assertLess(
                link.commands.index(explicit_feed), link.commands.index(rasters[2])
            )
            self.assertEqual(link.commands[-2:], [probe.GAP, probe.QUERY_MODE])
            self.assertTrue(state.gap_restored)

    def test_leading_rows_are_blank_and_preserve_marker_pixels(self):
        for letter, left in zip("ABC", (24, 120, 216)):
            plain = probe.marker(letter, left)
            padded = probe.marker(letter, left, 16)
            self.assertEqual(
                padded[:12], bytes.fromhex("1f 11 21 01 1d 76 30 00 28 00 20 00")
            )
            self.assertEqual(padded[12:652], b"\x00" * 640)
            self.assertEqual(padded[652:], plain[12:])

    def test_padded_pass_uses_same_height_for_all_three_rasters(self):
        link, state = FakeLink(), probe.State()
        probe.run_experiment(
            link, 8, state, lambda _: None, lambda _: None, leading_rows=16
        )
        rasters = [
            command for command in link.commands if command.startswith(probe.COPY_ONE)
        ]
        self.assertEqual(len(rasters), 3)
        self.assertTrue(all(len(payload) == 1292 for payload in rasters))
        self.assertEqual(len({payload[:12] for payload in rasters}), 1)
        self.assertTrue(state.gap_restored)

    def test_print_failure_restores_gap_without_retry_or_further_feed(self):
        link, state = FakeLink(fail_print=True), probe.State()
        with self.assertRaisesRegex(probe.ProbeError, "simulated print deadline"):
            probe.run_experiment(link, 8, state, lambda _: None, lambda _: None)
        self.assertEqual(state.rasters_submitted, 1)
        self.assertFalse(
            any(command.startswith(b"\x1b\x64") for command in link.commands)
        )
        self.assertEqual(link.commands[-2:], [probe.GAP, probe.QUERY_MODE])
        self.assertTrue(state.gap_restored)

    def test_alignment_feeds_precede_first_marker(self):
        link, state = FakeLink(), probe.State()
        probe.run_experiment(
            link, 8, state, lambda _: None, lambda _: None, align_label=True
        )
        rasters = [
            command for command in link.commands if command.startswith(probe.COPY_ONE)
        ]
        first = link.commands.index(rasters[0])
        self.assertLess(link.commands.index(bytes.fromhex("1b 64 01")), first)
        self.assertLess(link.commands.index(bytes.fromhex("1b 64 04")), first)
        self.assertEqual(link.commands.count(probe.GAP_PREAMBLE), 1)
        self.assertFalse(
            any(
                command in (probe.PREAMBLE, probe.GAP_PREAMBLE)
                for command in link.commands[first:]
            )
        )
        self.assertEqual(state.rasters_submitted, 3)
        self.assertTrue(state.gap_restored)

    def test_unconfirmed_continuous_mode_prints_nothing_and_restores_gap(self):
        link, state = FakeLink(ignore_continuous=True), probe.State()
        with self.assertRaisesRegex(probe.ProbeError, "Tracking mode not confirmed"):
            probe.run_experiment(link, 8, state, lambda _: None, lambda _: None)
        self.assertEqual(state.rasters_submitted, 0)
        self.assertEqual(link.commands[-2:], [probe.GAP, probe.QUERY_MODE])
        self.assertTrue(state.gap_restored)

    def test_failed_status_precondition_changes_nothing(self):
        status = READY.replace(bytes.fromhex("1a 06 89"), b"")
        link, state = FakeLink(status=status), probe.State()
        with self.assertRaisesRegex(probe.ProbeError, "stable paper present"):
            probe.run_experiment(link, 8, state, lambda _: None, lambda _: None)
        self.assertEqual(link.commands, [probe.STATUS])
        self.assertFalse(state.mode_change_attempted)
        self.assertFalse(state.gap_restored)
        self.assertEqual(state.rasters_submitted, 0)

    def test_failed_restore_does_not_replace_original_error(self):
        link = FakeLink(fail_print=True, drop_after_print_failure=True)
        state = probe.State()
        with self.assertRaisesRegex(probe.ProbeError, "simulated print deadline"):
            probe.run_experiment(link, 8, state, lambda _: None, lambda _: None)
        self.assertTrue(state.mode_change_attempted)
        self.assertFalse(state.gap_restored)

    def test_raster_send_failure_skips_same_link_restore(self):
        link, state = FakeLink(fail_raster_send=True), probe.State()
        with self.assertRaisesRegex(OSError, "partial raster write"):
            probe.run_experiment(link, 8, state, lambda _: None, lambda _: None)
        self.assertTrue(state.raster_write_incomplete)
        self.assertFalse(state.gap_restored)
        self.assertTrue(link.commands[-1].startswith(probe.COPY_ONE))
        self.assertEqual(state.rasters_submitted, 1)


class MainTests(unittest.TestCase):
    def test_mid_print_failure_restores_on_new_link_without_resending(self):
        first = FakeLink(fail_print=True, drop_after_print_failure=True)
        second = FakeLink()
        second.mode = probe.CONTINUOUS_REPLY
        code, output = run_main([first, second])
        self.assertEqual(code, 1)
        self.assertEqual(len(rasters(first.commands)), 1)
        self.assertEqual(second.commands, [probe.GAP, probe.QUERY_MODE])
        self.assertEqual(second.mode, probe.GAP_REPLY)
        self.assertIn("Stopped: ProbeError: simulated print deadline", output)
        self.assertIn("Gap restoration on this link failed", output)
        self.assertIn("Rasters submitted: 1; gap restored: True", output)
        self.assertNotIn("NOT confirmed", output)

    def test_raster_send_failure_restores_only_on_new_link(self):
        first = FakeLink(fail_raster_send=True)
        second = FakeLink()
        second.mode = probe.CONTINUOUS_REPLY
        code, output = run_main([first, second])
        self.assertEqual(code, 1)
        self.assertTrue(first.commands[-1].startswith(probe.COPY_ONE))
        self.assertNotIn(probe.GAP, first.commands)
        self.assertEqual(second.commands, [probe.GAP, probe.QUERY_MODE])
        self.assertIn("Stopped: OSError: simulated partial raster write", output)
        self.assertIn("gap restored: True", output)

    def test_unconfirmed_restore_warns_operator(self):
        first = FakeLink(fail_raster_send=True)
        second = FakeLink()
        second.dropped = True
        code, output = run_main([first, second])
        self.assertEqual(code, 1)
        self.assertEqual(second.commands, [])
        self.assertIn("Gap mode NOT confirmed: power-cycle the printer", output)


if __name__ == "__main__":
    unittest.main()
