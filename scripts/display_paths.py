"""Read active CCD display paths without changing monitor or driver settings.

Layouts and semantics: Windows SDK wingdi.h and Microsoft's QueryDisplayConfig
documentation. This is environment evidence, not a frame-rate measurement.
"""
import ctypes as c
import json

U32 = c.c_uint32


class Luid(c.Structure):
    _fields_ = [('low', U32), ('high', c.c_int32)]


class Ratio(c.Structure):
    _fields_ = [('n', U32), ('d', U32)]


class Region(c.Structure):
    _fields_ = [('width', U32), ('height', U32)]


class Signal(c.Structure):
    _fields_ = [('pixel_rate', c.c_uint64), ('h_sync', Ratio), ('v_sync', Ratio),
                ('active', Region), ('total', Region), ('standard', U32), ('scanline', U32)]


class Source(c.Structure):
    _fields_ = [('adapter', Luid), ('id', U32), ('mode', U32), ('status', U32)]


class Target(c.Structure):
    _fields_ = [('adapter', Luid), ('id', U32), ('mode', U32), ('technology', c.c_int32),
                ('rotation', U32), ('scaling', U32), ('refresh', Ratio), ('scanline', U32),
                ('available', c.c_int32), ('status', U32)]


class DisplayPath(c.Structure):
    _fields_ = [('source', Source), ('target', Target), ('flags', U32)]


class ModeValue(c.Union):
    _fields_ = [('signal', Signal), ('raw', c.c_byte * 48)]


class Mode(c.Structure):
    _fields_ = [('kind', U32), ('id', U32), ('adapter', Luid), ('value', ModeValue)]


class DeviceHeader(c.Structure):
    _fields_ = [('kind', U32), ('size', U32), ('adapter', Luid), ('id', U32)]


class TargetName(c.Structure):
    _fields_ = [('header', DeviceHeader), ('flags', U32), ('technology', c.c_int32),
                ('manufacturer', c.c_uint16), ('product', c.c_uint16), ('connector', U32),
                ('friendly_name', c.c_wchar * 64), ('device_path', c.c_wchar * 128)]


class SourceName(c.Structure):
    _fields_ = [('header', DeviceHeader), ('name', c.c_wchar * 32)]


def rate(value):
    return dict(n=value.n, d=value.d, hz=value.n / value.d if value.d else None)


def inspect():
    assert c.sizeof(DisplayPath) == 72 and c.sizeof(Mode) == 64
    user = c.WinDLL('user32')
    count, mode_count = U32(), U32()
    for _ in range(3):
        code = user.GetDisplayConfigBufferSizes(2, c.byref(count), c.byref(mode_count))
        if code:
            return dict(error=code, stage='GetDisplayConfigBufferSizes')
        paths = (DisplayPath * count.value)()
        modes = (Mode * mode_count.value)()
        code = user.QueryDisplayConfig(2, c.byref(count), paths, c.byref(mode_count), modes, None)
        if code != 122:
            break
    if code:
        return dict(error=code, stage='QueryDisplayConfig')
    result = []
    for path in paths[:count.value]:
        source = SourceName(DeviceHeader(1, c.sizeof(SourceName), path.source.adapter, path.source.id))
        target = TargetName(DeviceHeader(2, c.sizeof(TargetName), path.target.adapter, path.target.id))
        source_error = user.DisplayConfigGetDeviceInfo(c.byref(source))
        target_error = user.DisplayConfigGetDeviceInfo(c.byref(target))
        row = dict(source_name=source.name, source_name_error=source_error,
                   target_name=target.friendly_name, target_name_error=target_error,
                   target_name_flags=target.flags, target_available=bool(path.target.available),
                   output_technology=path.target.technology, target_status_flags=path.target.status,
                   path_flags=path.flags, refresh=rate(path.target.refresh))
        index = path.target.mode
        if index < mode_count.value and modes[index].kind == 2:
            signal = modes[index].value.signal
            row['signal'] = dict(pixel_rate=signal.pixel_rate, v_sync=rate(signal.v_sync),
                                 active=[signal.active.width, signal.active.height],
                                 total=[signal.total.width, signal.total.height])
        result.append(row)
    return dict(active_paths=result, read_only=True, api='QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)')


if __name__ == '__main__':
    print(json.dumps(inspect(), indent=2))
