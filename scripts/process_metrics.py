"""Portable subprocess wall time, CPU time and single-process peak resident memory."""
import ctypes
import os
import subprocess
import tempfile
import time
from bench_common import timed as unix_timed


def timed(command, cwd):
    if os.name != 'nt':
        return unix_timed(command, cwd)
    # Keep Popen's process handle alive after wait; no polling/sampling races.
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [('cb', wintypes.DWORD), ('faults', wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in (
                'peak', 'working', 'peak_paged', 'paged', 'peak_nonpaged',
                'nonpaged', 'pagefile', 'peak_pagefile')]

    get_memory = ctypes.WinDLL('psapi', use_last_error=True).GetProcessMemoryInfo
    get_memory.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    get_memory.restype = wintypes.BOOL
    get_times = ctypes.WinDLL('kernel32', use_last_error=True).GetProcessTimes
    get_times.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    get_times.restype = wintypes.BOOL
    with tempfile.TemporaryFile() as log:
        start = time.perf_counter()
        with subprocess.Popen(command, cwd=cwd, stdout=log, stderr=log) as child:
            try:
                status = child.wait(timeout=300)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
                raise
            elapsed = time.perf_counter() - start
            counters = Counters()
            counters.cb = ctypes.sizeof(counters)
            if not get_memory(int(child._handle), ctypes.byref(counters), counters.cb):
                raise ctypes.WinError(ctypes.get_last_error())
            times = [wintypes.FILETIME() for _ in range(4)]
            if not get_times(int(child._handle), *map(ctypes.byref, times)):
                raise ctypes.WinError(ctypes.get_last_error())
            kernel, user = ((t.dwHighDateTime << 32 | t.dwLowDateTime) / 1e7 for t in times[2:])
            if status:
                log.seek(0)
                raise RuntimeError(log.read().decode(errors='replace'))
    return {'seconds': elapsed, 'peak_rss_bytes': counters.peak, 'cpu_seconds': kernel + user}

