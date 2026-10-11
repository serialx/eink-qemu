#!/usr/bin/env python3
"""Extract temporary TURBO instrumentation snapshots; no source edits required."""
import argparse
import json
from pathlib import Path
import re


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('logs',nargs='+',type=Path)
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args()
    result={}
    for log in args.logs:
        text=log.read_text(errors='replace')
        snapshots=[json.loads(s) for s in re.findall(r'^TURBO_COUNTERS (.+)$',text,re.M)]
        if len(snapshots)!=2 or [s['phase'] for s in snapshots]!=[0,1]:
            raise RuntimeError(f'{log}: expected Home and settled snapshots')
        asyncify=[json.loads(s) for s in re.findall(r'^TURBO_ASYNCIFY (.+)$',text,re.M)]
        asyncify += [json.loads(f.read_text()) for f in log.parent.glob('asyncify-*.json')]
        for snapshot in snapshots:
            snapshot['asyncify']=[s for s in asyncify if s['phase']==snapshot['phase']]
        result[log.parent.name]={'phases':snapshots,
            'sd_prefix':[list(map(int,s)) for s in re.findall(r'^TURBO_SD (\d+) (\d+) (\d+)$',text,re.M)]}
    args.output.write_text(json.dumps(result,indent=2)+'\n')
    print(args.output)


if __name__=='__main__':main()
