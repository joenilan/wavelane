#!/usr/bin/env python3
"""Run a command under a test-owned 1280x800 virtual Wayland compositor.

Usage: python3 scripts/private_benchmark_session.py python3 scripts/benchmark_backends.py label=/absolute/binary
No windows appear on the owner's desktop. Only test-owned processes are stopped.
"""
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / 'local/gpui-evaluation'
if sys.argv[1:] == ['--compositor-child']:
    (BASE/'private-bus.txt').write_text(os.environ['DBUS_SESSION_BUS_ADDRESS'])
    os.execvp('kwin_wayland',['kwin_wayland','--virtual','--width','1280','--height','800',
              '--scale','1','--socket','wayland-bench','--no-lockscreen',
              '--no-global-shortcuts','--no-kactivities','--inputmethod','/bin/true'])
if len(sys.argv)<2 or sys.argv[1:] in [['--help'],['-h']]:
    print(__doc__)
    raise SystemExit(0)
for name in ['runtime','config','cache']: (BASE/name).mkdir(parents=True,exist_ok=True)
(BASE/'runtime').chmod(0o700)
if (BASE/'runtime/wayland-bench').is_socket():
    raise SystemExit('Private compositor already exists; use benchmark_backends.py directly.')
env=os.environ.copy(); env.pop('DISPLAY',None)
env.update(XDG_RUNTIME_DIR=str(BASE/'runtime'),XDG_CONFIG_HOME=str(BASE/'config'),
           XDG_CACHE_HOME=str(BASE/'cache'),WAYLAND_DISPLAY='wayland-bench')
with (BASE/'private-compositor.log').open('w') as log:
    compositor=subprocess.Popen(['dbus-run-session',sys.executable,__file__,'--compositor-child'],
                                env=env,stdout=log,stderr=log,start_new_session=True)
    try:
        for _ in range(100):
            if (BASE/'runtime/wayland-bench').is_socket(): break
            if compositor.poll() is not None: raise RuntimeError('Private compositor exited')
            time.sleep(.1)
        else: raise RuntimeError('Private compositor did not create its socket')
        env['DBUS_SESSION_BUS_ADDRESS']=(BASE/'private-bus.txt').read_text()
        status=subprocess.run(sys.argv[1:],cwd=ROOT,env=env,check=False).returncode
    finally:
        if compositor.poll() is None:
            os.killpg(compositor.pid,signal.SIGTERM)
            try: compositor.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(compositor.pid,signal.SIGKILL); compositor.wait()
raise SystemExit(status)
