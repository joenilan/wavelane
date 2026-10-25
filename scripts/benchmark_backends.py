#!/usr/bin/env python3
"""Run synthetic scenes only inside an existing PRIVATE evaluation compositor.

Build with --release --features perf-harness[,gpui-fast]. Pass copied binaries
as label=/absolute/path. Never launches the live engine or registers shortcuts.
GPU watts/utilization are whole-device, pmon rows are per process; zero/absent
pmon values are below resolution/unsupported, not proof of zero GPU work.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / 'local/gpui-evaluation'

def run(label, binary, scene, seconds, iteration):
    folder = BASE / 'measurements' / f'{label}-{scene}-{iteration}'
    folder.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    env.pop('DISPLAY', None)
    env.update(XDG_RUNTIME_DIR=str(BASE/'runtime'), XDG_CONFIG_HOME=str(BASE/'config'),
               XDG_CACHE_HOME=str(BASE/'cache'), WAYLAND_DISPLAY='wayland-bench',
               DBUS_SESSION_BUS_ADDRESS=(BASE/'private-bus.txt').read_text(),
               WAVELANE_BENCHMARK_PRIVATE='1', WAVELANE_BENCH_SCENE=scene,
               WAVELANE_BENCH_SECONDS=str(seconds))
    assert (BASE/'runtime/wayland-bench').is_socket(), 'Start private KWin first'
    samples = []
    with (folder/'app.log').open('w') as log, (folder/'pmon.log').open('w') as gpu_log:
        pmon = subprocess.Popen(['nvidia-smi','pmon','-s','um','-d','1','-o','DT'], stdout=gpu_log, stderr=subprocess.STDOUT)
        app = subprocess.Popen([str(binary),'--benchmark'], env=env, stdout=log, stderr=subprocess.STDOUT)
        start = time.monotonic()
        wall_start = time.time()
        previous = None
        try:
            while app.poll() is None:
                now = time.monotonic()
                if now-start > seconds+45:
                    raise RuntimeError(f'{label}/{scene} timed out')
                try:
                    fields = Path(f'/proc/{app.pid}/stat').read_text().rsplit(')',1)[1].split()
                    ticks = int(fields[11])+int(fields[12])
                    rss = int(fields[21])*os.sysconf('SC_PAGE_SIZE')/2**20
                except FileNotFoundError:
                    break
                whole = subprocess.run(['nvidia-smi','--query-gpu=utilization.gpu,power.draw','--format=csv,noheader,nounits'],capture_output=True,text=True,check=True).stdout.strip()
                util, power = [float(v.strip()) for v in whole.split(',')]
                if previous:
                    dt = now-previous[0]
                    cpu = (ticks-previous[1])/os.sysconf('SC_CLK_TCK')/dt*100
                    if now-start >= 6:
                        samples.append(dict(elapsed_s=now-start,cpu_one_core_pct=cpu,rss_mib=rss,whole_gpu_pct=util,whole_gpu_watts=power))
                previous=(now,ticks)
                time.sleep(max(0, 1-(time.monotonic()-now)))
            app.wait(timeout=5)
            if app.returncode:
                raise RuntimeError(f'{label}/{scene} exited {app.returncode}: {folder / "app.log"}')
        finally:
            if app.poll() is None:
                app.terminate()
                try: app.wait(timeout=5)
                except subprocess.TimeoutExpired: app.kill(); app.wait()
            pmon.terminate(); pmon.wait(timeout=5)
    lines = (folder/'app.log').read_text().splitlines()
    results = [json.loads(line.split('BENCH_RESULT ',1)[1]) for line in lines if 'BENCH_RESULT ' in line]
    if len(results) != 1 or not samples:
        raise RuntimeError(f'Missing benchmark result or samples: {folder}')
    result=results[0]
    result.update(label=label,iteration=iteration,pid=app.pid,binary=str(binary),binary_sha256=hashlib.file_digest(binary.open('rb'),'sha256').hexdigest(),samples=len(samples))
    for key in samples[0]:
        if key!='elapsed_s': result[key+'_mean']=sum(x[key] for x in samples)/len(samples)
    rows=[]
    for line in (folder/'pmon.log').read_text().splitlines():
        f=line.split()
        if len(f)>12 and f[3]==str(app.pid):
            stamp=time.mktime(time.strptime(f[0]+' '+f[1],'%Y%m%d %H:%M:%S'))
            # pmon describes the previous second. Exclude startup/fade samples.
            if wall_start+6 <= stamp <= wall_start+5+seconds:
                rows.append(f[2:])
    result['pmon_process_rows_in_sample_window']=len(rows)
    result['pmon_sample_scope']='app process only; prior-second samples after warm-up; missing is unknown'
    for key,column in [('app_sm_pct',3),('app_mem_busy_pct',4),('app_vram_mib',9)]:
        values=[float(row[column]) for row in rows if row[column]!='-']
        result[key+'_mean']=sum(values)/len(values) if values else None
        result[key+'_valid_samples']=len(values)
    (folder/'result.json').write_text(json.dumps(result,indent=2)+'\n')
    with (folder/'samples.csv').open('w') as f:
        writer=csv.DictWriter(f,fieldnames=samples[0]);writer.writeheader();writer.writerows(samples)
    print(json.dumps(result),flush=True)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binaries',nargs='+',help='label=/absolute/path')
    parser.add_argument('--scenes',default='tray,flyout,map-idle,map-music,interaction,close-map')
    parser.add_argument('--seconds',type=int,default=20)
    parser.add_argument('--repeats',type=int,default=2)
    args=parser.parse_args()
    binaries=[]
    for item in args.binaries:
        label,path=item.split('=',1); binary=Path(path).resolve(strict=True)
        assert label and all(c.isalnum() or c in '-_' for c in label)
        binaries.append((label,binary))
    hashes=[hashlib.file_digest(binary.open('rb'),'sha256').hexdigest() for _,binary in binaries]
    if len(set(hashes)) != len(hashes):
        raise SystemExit('Refusing differently labelled builds with identical binary hashes.')
    for iteration in range(args.repeats):
        # Alternate order to limit systematic order bias; never parallelize loads.
        order=binaries if iteration%2==0 else list(reversed(binaries))
        for scene in args.scenes.split(','):
            for label,binary in order: run(label,binary,scene,args.seconds,iteration)
if __name__=='__main__': main()
