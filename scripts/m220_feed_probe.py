#!/usr/bin/env python3

"""Measure M220 continuous-mode ESC d feed using three printed reference marks.

Dry-run by default. With --print and --address, print A and B without explicit
feed, then ESC d 4 or 8 and C. Measure the top-line spacings along the intact
backing strip: extra feed = BC - AB. Each marker body is 320 x 16 dots,
preceded by 16 blank rows by default (`--leading-rows 0` for the original
unpadded test).

Uses Linux Bluetooth RFCOMM and the Python standard library. See
docs/m220-positioning.md for the measured behavior and remaining uncertainties.
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass

WIDTH_BYTES = 40
HEIGHT = 16
STATUS = bytes.fromhex("1f 11 07 1f 11 12 1f 11 13 1f 11 11")
QUERY_MODE = bytes.fromhex("1f 11 19")
GAP = bytes.fromhex("1f 11 0a")
CONTINUOUS = bytes.fromhex("1f 11 0b")
GAP_REPLY = bytes.fromhex("1a 0c 0a")
CONTINUOUS_REPLY = bytes.fromhex("1a 0c 0b")
PREAMBLE = bytes.fromhex("1f 11 24 20 1f 11 0b 1b 40")
GAP_PREAMBLE = bytes.fromhex("1f 11 24 20 1f 11 0a 1b 40")
COPY_ONE = bytes.fromhex("1f 11 21 01")
PRINTED = bytes.fromhex("1a 0f 0c")
GLYPHS = {
    "A": ("01110", "10001", "10001", "11111", "10001", "10001", "10001"),
    "B": ("11110", "10001", "10001", "11110", "10001", "10001", "11110"),
    "C": ("01111", "10000", "10000", "10000", "10000", "10000", "01111"),
}


class ProbeError(RuntimeError):
    """A failed precondition, print, or mode restoration; never retry a print."""


@dataclass
class State:
    mode_change_attempted: bool = False
    gap_restored: bool = False
    rasters_submitted: int = 0
    # A raster write that raised may have partially reached the printer, which
    # would then consume further bytes on that link as image data.
    raster_write_incomplete: bool = False


def marker(letter: str, left: int, leading_rows: int = 0) -> bytes:
    """One marker body plus blank leading rows, in a distinct horizontal lane."""
    if leading_rows not in (0, 16):
        raise ValueError("Only 0 or 16 blank leading rows are supported")
    height = HEIGHT + leading_rows
    data = bytearray(WIDTH_BYTES * height)

    def dot(x: int, y: int) -> None:
        if not (0 <= x < WIDTH_BYTES * 8 and 0 <= y < HEIGHT):
            raise ValueError("Marker pixel outside raster")
        data[(y + leading_rows) * WIDTH_BYTES + x // 8] |= 0x80 >> (x % 8)

    for y in range(2):
        for x in range(left, left + 80):
            dot(x, y)
    for gy, row in enumerate(GLYPHS[letter]):
        for gx, pixel in enumerate(row):
            if pixel == "1":
                for dy in range(2):
                    for dx in range(2):
                        dot(left + 34 + gx * 2 + dx, 2 + gy * 2 + dy)
    header = bytes.fromhex("1d 76 30 00") + struct.pack("<HH", WIDTH_BYTES, height)
    return COPY_ONE + header + data


class Link:
    def __init__(self, sock: socket.socket, log: Callable[[str], None]) -> None:
        self.sock = sock
        self.log = log

    def send(self, command: bytes) -> None:
        self.sock.settimeout(5)
        for offset in range(0, len(command), 1024):
            self.sock.sendall(command[offset : offset + 1024])

    def receive(self, seconds: float, *, printed: bool = False) -> bytes:
        """Absolute deadline, including when unsolicited status keeps arriving."""
        deadline = time.monotonic() + seconds
        result = bytearray()
        while time.monotonic() < deadline:
            self.sock.settimeout(max(0.001, deadline - time.monotonic()))
            try:
                chunk = self.sock.recv(1024)
            except TimeoutError:
                break
            if not chunk:
                raise ProbeError("Printer disconnected; no automatic print retry")
            result.extend(chunk)
            self.log("RX " + chunk.hex(" "))
            if printed:
                # Completion may span TCP/RFCOMM receive boundaries. This is
                # the same three-byte report used by the application's driver.
                for index in range(len(result) - 2):
                    if result[index : index + 2] == b"\x1a\x0f":
                        if result[index + 2] != 0x0C:
                            raise ProbeError("Printer reported print failure")
                        return bytes(result)
                if b"\x1a\x0b\xb8" in result:
                    raise ProbeError("Printer cancelled the print")
        if printed:
            raise ProbeError("Print-complete deadline expired; no further raster sent")
        return bytes(result)


@contextmanager
def connect(address: str, log: Callable[[str], None]) -> Iterator[Link]:
    sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_STREAM, socket.BTPROTO_RFCOMM)
    try:
        sock.settimeout(5)
        sock.connect((address, 1))
        yield Link(sock, log)
    finally:
        sock.close()


def check_mode(link: Link, expected: bytes, log: Callable[[str], None]) -> None:
    link.send(QUERY_MODE)
    reply = link.receive(1)
    if expected not in reply:
        raise ProbeError(
            f"Tracking mode not confirmed: expected {expected.hex(' ')}, "
            f"got {reply.hex(' ')}"
        )
    log("Tracking mode confirmed: " + expected.hex(" "))


def restore_gap(link: Link, state: State, log: Callable[[str], None]) -> None:
    log("Restore gap mode: " + GAP.hex(" "))
    link.send(GAP)
    check_mode(link, GAP_REPLY, log)
    state.gap_restored = True


def run_experiment(
    link: Link,
    feed: int,
    state: State,
    log: Callable[[str], None],
    pause: Callable[[float], None] = time.sleep,
    *,
    align_label: bool = False,
    leading_rows: int = 16,
) -> None:
    if feed not in (4, 8):
        raise ValueError("Only feed counts 4 and 8 are supported")
    link.receive(0.5)
    link.send(STATUS)
    status = link.receive(2)
    required = ("1a 07 03 00 01", "1a 05 98", "1a 03 a8", "1a 06 89")
    if (
        not all(bytes.fromhex(item) in status for item in required)
        or b"\x1a\x06\x88" in status
    ):
        raise ProbeError(
            "Expected firmware 3.0.1, closed lid, normal temperature "
            "and stable paper present"
        )
    check_mode(link, GAP_REPLY, log)
    try:
        # Set before sending, so a partially transmitted setup still triggers
        # restoration. The margin and reset match the original validated flow.
        state.mode_change_attempted = True
        if align_label:
            log("Set margin 32, gap mode, initial reset: " + GAP_PREAMBLE.hex(" "))
            link.send(GAP_PREAMBLE)
            pause(0.1)
            check_mode(link, GAP_REPLY, log)
            log("Establish fresh label reference: gap-mode ESC d 1 (1b 64 01)")
            link.send(bytes.fromhex("1b 64 01"))
            link.receive(6)
            link.send(CONTINUOUS)
            pause(0.1)
            check_mode(link, CONTINUOUS_REPLY, log)
            log(
                "Advance header before A: continuous ESC d 4 (1b 64 04), "
                "nominally ~2 mm"
            )
            link.send(bytes.fromhex("1b 64 04"))
            link.receive(2)
        else:
            log("Set margin 32, continuous mode, initial reset: " + PREAMBLE.hex(" "))
            link.send(PREAMBLE)
            pause(0.1)
            check_mode(link, CONTINUOUS_REPLY, log)
        for letter, left in zip("ABC", (24, 120, 216)):
            if letter == "C":
                command = bytes((0x1B, 0x64, feed))
                log(f"Feed ESC d {feed}: {command.hex(' ')}")
                link.send(command)
                link.receive(2)
            payload = marker(letter, left, leading_rows)
            log(
                f"Print {letter}: 320 x {HEIGHT + leading_rows} dots, "
                f"{len(payload)} bytes including copy/header"
            )
            # Count attempts, not acknowledgements: a failed write can have
            # partially reached the printer, so it must not be repeated.
            state.rasters_submitted += 1
            try:
                link.send(payload)
            except BaseException:
                state.raster_write_incomplete = True
                raise
            link.receive(30, printed=True)
            log(f"{letter}: print complete")
            link.receive(0.5)
    except BaseException:
        # Never let a failed restoration replace the original error. If it
        # fails, gap_restored stays False and main() reconnects to restore.
        if state.raster_write_incomplete:
            log(
                "Raster write incomplete; skip restoration on this link, "
                "where it could be consumed as image data"
            )
        else:
            try:
                restore_gap(link, state, log)
            except (OSError, ProbeError) as restore_error:
                log(f"Gap restoration on this link failed: {restore_error}")
        raise
    else:
        restore_gap(link, state, log)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--feed",
        type=int,
        choices=(4, 8),
        default=8,
        help="ESC d count before C (default: 8)",
    )
    parser.add_argument(
        "--address", help="paired M220 Bluetooth MAC; required with --print"
    )
    parser.add_argument(
        "--leading-rows",
        type=int,
        choices=(0, 16),
        default=16,
        help=(
            "blank rows before each marker (default: 16); "
            "use 0 for the original unpadded test"
        ),
    )
    parser.add_argument(
        "--align-label",
        action="store_true",
        help="reproduce the failed gap-reference/header setup; may advance a label",
    )
    parser.add_argument(
        "--print",
        dest="send",
        action="store_true",
        help="connect and print; otherwise dry-run",
    )
    args = parser.parse_args(argv)
    if args.send and not args.address:
        parser.error("--print requires --address")
    start = time.monotonic()

    def log(message: str) -> None:
        print(f"{time.monotonic() - start:7.3f}s {message}", flush=True)

    log(
        f"Three 320 x {HEIGHT + args.leading_rows}-dot rasters; "
        f"ESC d {args.feed} only between B and C"
    )
    log(f"Reference lines start at row {args.leading_rows} inside each raster")
    if args.align_label:
        log(
            "Setup before A: gap ESC d 1, continuous ESC d 4 header; "
            "neither is part of AB or BC"
        )
    for letter, left in zip("ABC", (24, 120, 216)):
        payload = marker(letter, left, args.leading_rows)
        log(
            f"{letter}: lane starts at dot {left}; "
            f"copy/header {payload[:12].hex(' ')}; "
            f"{len(payload) - 12} raster bytes"
        )
    if not args.send:
        log("DRY RUN: no connection, mode change, feed, or print")
        return 0
    state = State()
    failed = False
    try:
        with connect(args.address, log) as link:
            run_experiment(
                link,
                args.feed,
                state,
                log,
                align_label=args.align_label,
                leading_rows=args.leading_rows,
            )
    except (OSError, ProbeError, KeyboardInterrupt) as error:
        failed = True
        log(f"Stopped: {type(error).__name__}: {error}")
    finally:
        if state.mode_change_attempted and not state.gap_restored:
            try:
                log(
                    "Reconnect only to restore tracking; "
                    "do not resend any raster or feed"
                )
                with connect(args.address, log) as link:
                    restore_gap(link, state, log)
            except (OSError, ProbeError, KeyboardInterrupt) as error:
                failed = True
                log(
                    "Gap restoration could not be confirmed: "
                    f"{type(error).__name__}: {error}"
                )
    if state.mode_change_attempted and not state.gap_restored:
        log(
            "Gap mode NOT confirmed: power-cycle the printer and confirm "
            "the tracking mode in the app before printing."
        )
    log(
        f"Rasters submitted: {state.rasters_submitted}; "
        f"gap restored: {state.gap_restored}"
    )
    if not failed:
        log(
            "Measure AB and BC; extra feed = BC - AB; "
            f"estimated mm/unit = (BC - AB) / {args.feed}"
        )
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
