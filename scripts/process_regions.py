"""Read-only region accounting for an explicitly selected validation process."""
import argparse
import collections
import ctypes as c
import json
from ctypes import wintypes as w

class Region(c.Structure):
    _fields_=[('BaseAddress',c.c_void_p),('AllocationBase',c.c_void_p),('AllocationProtect',w.DWORD),('PartitionId',w.WORD),('RegionSize',c.c_size_t),('State',w.DWORD),('Protect',w.DWORD),('Type',w.DWORD)]

def inspect(pid):
    kernel=c.WinDLL('kernel32',use_last_error=True)
    kernel.OpenProcess.argtypes=[w.DWORD,w.BOOL,w.DWORD]; kernel.OpenProcess.restype=w.HANDLE
    kernel.VirtualQueryEx.argtypes=[w.HANDLE,c.c_void_p,c.POINTER(Region),c.c_size_t]; kernel.VirtualQueryEx.restype=c.c_size_t
    kernel.CloseHandle.argtypes=[w.HANDLE]
    handle=kernel.OpenProcess(0x400,False,pid)
    if not handle: raise c.WinError(c.get_last_error())
    address=0; committed=collections.Counter(); private_blocks=collections.Counter()
    try:
        while address < 0x7fffffffffff:
            info=Region()
            if not kernel.VirtualQueryEx(handle,address,c.byref(info),c.sizeof(info)): break
            if info.State==0x1000:
                committed[str(info.Type)]+=info.RegionSize
                if info.Type==0x20000: private_blocks[int(info.AllocationBase or 0)]+=info.RegionSize
            next_address=int(info.BaseAddress or 0)+info.RegionSize
            if next_address<=address: break
            address=next_address
    finally: kernel.CloseHandle(handle)
    return dict(pid=pid,committed_by_type=dict(committed),private_block_sizes=sorted(private_blocks.values(),reverse=True),type_labels={'131072':'private','262144':'mapped','16777216':'image'})

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('pid',type=int);a=p.parse_args();print(json.dumps(inspect(a.pid)))
