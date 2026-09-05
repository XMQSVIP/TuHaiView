"""Isolated read-only DXGI vertical-blank timing, not product acceptance.

COM layouts/slots follow the Windows SDK dxgi.h IDXGIFactory1/IDXGIAdapter/
IDXGIOutput vtables. Creates no device, swapchain, window or rendered content.
Run separately from formal graphics benchmarks; an OS wait can be delayed by
scheduling and does not measure physical photons.
"""
import argparse
import ctypes as c
import json
import math
import statistics
import time
import uuid


class Guid(c.Structure):
    _fields_ = [('bytes', c.c_byte * 16)]


class Rect(c.Structure):
    _fields_ = [(n, c.c_int32) for n in ['left', 'top', 'right', 'bottom']]


class Description(c.Structure):
    _fields_ = [('name', c.c_wchar * 32), ('rect', Rect), ('attached', c.c_int32),
                ('rotation', c.c_int32), ('monitor', c.c_void_p)]


def invoke(pointer, slot, restype, args, *values):
    table = c.cast(pointer, c.POINTER(c.POINTER(c.c_void_p))).contents
    return c.WINFUNCTYPE(restype, c.c_void_p, *args)(table[slot])(pointer, *values)


def check(code):
    if code < 0:
        raise OSError(f'DXGI HRESULT 0x{code & 0xffffffff:08x}')


def release(pointer):
    if pointer:
        invoke(pointer, 2, c.c_uint32, [])


def probe(samples):
    factory, adapter, output = c.c_void_p(), c.c_void_p(), c.c_void_p()
    guid = Guid.from_buffer_copy(uuid.UUID('770aae78-f26f-4dba-a829-253c83d1b387').bytes_le)
    dxgi = c.WinDLL('dxgi')
    dxgi.CreateDXGIFactory1.argtypes = [c.POINTER(Guid), c.POINTER(c.c_void_p)]
    dxgi.CreateDXGIFactory1.restype = c.c_int32
    try:
        check(dxgi.CreateDXGIFactory1(c.byref(guid), c.byref(factory)))
        check(invoke(factory, 12, c.c_int32, [c.c_uint32, c.POINTER(c.c_void_p)], 0, c.byref(adapter)))
        check(invoke(adapter, 7, c.c_int32, [c.c_uint32, c.POINTER(c.c_void_p)], 0, c.byref(output)))
        description = Description()
        check(invoke(output, 7, c.c_int32, [c.POINTER(Description)], c.byref(description)))
        values = []
        for i in range(samples + 2):
            start = time.perf_counter_ns()
            check(invoke(output, 10, c.c_int32, []))
            elapsed = (time.perf_counter_ns() - start) / 1e6
            if i >= 2:
                values.append(elapsed)
        ordered = sorted(values)
        return dict(diagnostic_only=True, method='IDXGIOutput::WaitForVBlank',
                    output=description.name, attached=bool(description.attached),
                    intervals_ms=values, summary=dict(n=len(values), median=statistics.median(values),
                    p95=ordered[math.ceil(len(values) * .95) - 1],
                    p99=ordered[math.ceil(len(values) * .99) - 1], maximum=max(values)))
    finally:
        release(output)
        release(adapter)
        release(factory)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--samples', type=int, default=180)
    args = parser.parse_args()
    if not 30 <= args.samples <= 600:
        parser.error('samples must be between 30 and 600')
    print(json.dumps(probe(args.samples), indent=2))
