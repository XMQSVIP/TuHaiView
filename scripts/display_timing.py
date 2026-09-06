"""Read-only DWM composition timing; never changes display/session settings."""
import ctypes as c
import json

class Ratio(c.Structure):
    _pack_=1
    _fields_=[('numerator',c.c_uint32),('denominator',c.c_uint32)]

class Timing(c.Structure):
    _pack_=1
    _fields_=[('cbSize',c.c_uint32),('rateRefresh',Ratio),('qpcRefreshPeriod',c.c_uint64),('rateCompose',Ratio),('qpcVBlank',c.c_uint64),('cRefresh',c.c_uint64),('cDXRefresh',c.c_uint32),('qpcCompose',c.c_uint64),('cFrame',c.c_uint64),('cDXPresent',c.c_uint32),('cRefreshFrame',c.c_uint64),('cFrameSubmitted',c.c_uint64),('cDXPresentSubmitted',c.c_uint32),('cFrameConfirmed',c.c_uint64),('cDXPresentConfirmed',c.c_uint32),('cRefreshConfirmed',c.c_uint64),('cDXRefreshConfirmed',c.c_uint32),('cFramesLate',c.c_uint64),('cFramesOutstanding',c.c_uint32)]+[(n,c.c_uint64) for n in ['cFrameDisplayed','qpcFrameDisplayed','cRefreshFrameDisplayed','cFrameComplete','qpcFrameComplete','cFramePending','qpcFramePending','cFramesDisplayed','cFramesComplete','cFramesPending','cFramesAvailable','cFramesDropped','cFramesMissed','cRefreshNextDisplayed','cRefreshNextPresented','cRefreshesDisplayed','cRefreshesPresented','cRefreshStarted','cPixelsReceived','cPixelsDrawn','cBuffersEmpty']]

def timing():
    t=Timing(); t.cbSize=c.sizeof(t)
    result=c.windll.dwmapi.DwmGetCompositionTimingInfo(None,c.byref(t))
    frequency=c.c_int64(); c.windll.kernel32.QueryPerformanceFrequency(c.byref(frequency))
    return dict(qpc_frequency=frequency.value,hresult=hex(result&0xffffffff),size=t.cbSize,refresh_n=t.rateRefresh.numerator,refresh_d=t.rateRefresh.denominator,compose_n=t.rateCompose.numerator,compose_d=t.rateCompose.denominator,refresh_period_ms=t.qpcRefreshPeriod/frequency.value*1000,remote_session=bool(c.windll.user32.GetSystemMetrics(0x1000)))

if __name__=='__main__': print(json.dumps(timing()))
