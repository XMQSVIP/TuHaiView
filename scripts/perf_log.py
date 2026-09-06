"""Read legacy samples and lossless schema-v4 compact batches."""
import json

COLUMNS=('monotonic_us','qpc','frame_id','scenario','request_id','generation','name','value','frame_known','render','time_ms')

def expand(record):
    if record.get('kind')!='sample_batch':
        yield record
        return
    rows=record.get('rows')
    if not isinstance(rows,list) or not 1<=len(rows)<=64:
        raise ValueError('invalid sample batch size')
    for row in rows:
        if not isinstance(row,list) or len(row)!=len(COLUMNS):
            raise ValueError('invalid sample batch row')
        yield dict(zip(COLUMNS,row))

def read_samples(source):
    for line in source:
        yield from expand(json.loads(line))
