"""Inspect PE dependencies; this cannot replace clean Windows startup testing."""
import argparse
import hashlib
import json
from pathlib import Path
import pefile

SYSTEM_IMPORTS = {
    'advapi32.dll', 'bcryptprimitives.dll', 'combase.dll', 'd3dcompiler_47.dll',
    'dwmapi.dll', 'gdi32.dll', 'imm32.dll', 'kernel32.dll', 'ntdll.dll',
    'ole32.dll', 'oleaut32.dll', 'opengl32.dll', 'psapi.dll', 'shell32.dll',
    'shlwapi.dll', 'uiautomationcore.dll', 'user32.dll', 'uxtheme.dll', 'winmm.dll',
    'api-ms-win-core-synch-l1-2-0.dll',
}


def inspect(path, revision):
    data = path.read_bytes()
    pe = pefile.PE(data=data, fast_load=True)
    pe.parse_data_directories(directories=[
        pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_IMPORT'],
        pefile.DIRECTORY_ENTRY['IMAGE_DIRECTORY_ENTRY_DELAY_IMPORT']])
    imports = sorted({item.dll.decode('ascii').lower() for attr in
                      ['DIRECTORY_ENTRY_IMPORT', 'DIRECTORY_ENTRY_DELAY_IMPORT']
                      for item in getattr(pe, attr, [])})
    unknown = sorted(set(imports) - SYSTEM_IMPORTS)
    report = dict(executable=str(path.resolve()), bytes=len(data), sha256=hashlib.sha256(data).hexdigest().upper(),
                  source_commit=revision, machine=hex(pe.FILE_HEADER.Machine),
                  subsystem=pe.OPTIONAL_HEADER.Subsystem, imports=imports,
                  imports_outside_reviewed_system_list=unknown,
                  static_dependency_check_passed=bool(imports) and not unknown and pe.FILE_HEADER.Machine == 0x8664,
                  clean_windows_10_11_tested=False,
                  limitation='Static normal and delay import review only. Dynamic driver/runtime loads and OS compatibility need separate startup tests.')
    pe.close()
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('executable', type=Path)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = inspect(args.executable, args.source_revision)
    args.output.write_text(json.dumps(result, indent=2), encoding='utf-8')
    print(json.dumps(result))
    if not result['static_dependency_check_passed']:
        raise SystemExit(1)
