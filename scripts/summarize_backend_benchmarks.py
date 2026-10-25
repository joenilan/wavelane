#!/usr/bin/env python3
"""Summarize matched backend run JSONL without treating missing GPU samples as zero."""
import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('results',type=Path)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
groups=defaultdict(list)
for line in args.results.read_text().splitlines():
    if line.strip():
        row=json.loads(line)
        groups[row['label'],row['scene']].append(row)
summary=[]
for (label,scene),runs in sorted(groups.items()):
    out=dict(label=label,scene=scene,runs=len(runs),sample_seconds=sum(r['sample_s'] for r in runs),
             frames=sum(r.get('frames',0) for r in runs),view_renders=sum(r.get('view_renders',0) for r in runs),
             metering_values=sorted(set(r['metering'] for r in runs)),
             window_counts=sorted(set(r['windows'] for r in runs)),
             commands=sum(r['commands'] for r in runs))
    out['draws_per_second']=out['frames']/out['sample_seconds']
    for field in ['cpu_one_core_pct_mean','rss_mib_mean','whole_gpu_pct_mean','whole_gpu_watts_mean',
                  'draw_p50_ms','draw_p95_ms','present_interval_p50_ms']:
        vals=[r[field] for r in runs if r.get(field) is not None]
        out['mean_run_'+field]=sum(vals)/len(vals) if vals else None
        out['min_run_'+field]=min(vals) if vals else None
        out['max_run_'+field]=max(vals) if vals else None
    for field in ['app_sm_pct','app_mem_busy_pct','app_vram_mib']:
        count=sum(r[field+'_valid_samples'] for r in runs)
        out[field+'_valid_samples']=count
        out[field+'_mean']=sum(r[field+'_mean']*r[field+'_valid_samples'] for r in runs if r[field+'_mean'] is not None)/count if count else None
    out['pmon_process_rows']=sum(r.get('pmon_process_rows_in_sample_window',0) for r in runs)
    summary.append(out)
args.output.mkdir(parents=True,exist_ok=True)
result={'method':{'warmup_seconds':5,'resolution':'1280x800 map, 380x657 flyout, scale 1',
 'scene':'six synthetic outputs and sixteen apps; no live audio routing/capture',
 'cpu':'100% = one logical core; process CPU and RSS sampled from /proc',
 'draw_times':'CPU Window::draw histogram; mean of run medians/p95s, not a pooled percentile; no GPU execution timing',
 'gpu':'NVIDIA pmon is per-process, coarse and sparse; null means unavailable/below resolution, never zero',
 'watts':'whole-device only; uncontrolled live desktop and compositor activity prevent app power attribution',
 'order':'all builds sequential, second pass reversed; same optimized release flags and scene scheduling per source version',
 'limitations':['Synthetic levels omit native audio capture CPU cost.','No Intel/AMD laptop or battery measurements.',
 'Private compositor confirms blur protocol requests, not real desktop tray activation, raising or shortcut bindings.',
 'Closed windows may leave GPU allocations and app caches resident even though draws and meters stop.']},'summary':summary}
(args.output/'comparison.json').write_text(json.dumps(result,indent=2)+'\n')
if summary:
    with (args.output/'comparison.csv').open('w') as f:
        writer=csv.DictWriter(f,fieldnames=summary[0]);writer.writeheader();writer.writerows(summary)
for row in summary:
    print(f"{row['label']:20} {row['scene']:12} n={row['runs']} CPU={row['mean_run_cpu_one_core_pct_mean']:.2f}% RSS={row['mean_run_rss_mib_mean']:.1f}MiB draw/s={row['draws_per_second']:.1f} p95={row['mean_run_draw_p95_ms']} GPU={row['app_sm_pct_mean']} ({row['app_sm_pct_valid_samples']}/{row['pmon_process_rows']}) W={row['mean_run_whole_gpu_watts_mean']:.1f} meters={row['metering_values']}")
