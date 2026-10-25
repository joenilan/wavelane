#!/usr/bin/env python3
"""Verify native meter teardown using an isolated PipeWire server and null sink."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BASE = (ROOT / 'local/gpui-evaluation').resolve()
BASE.mkdir(parents=True, exist_ok=True)
# UNIX socket paths have a small fixed limit; /tmp also supports long worktree paths.
with tempfile.TemporaryDirectory(prefix='wavelane-meter-') as directory:
    runtime = Path(directory)
    env = os.environ.copy()
    env.update(PIPEWIRE_RUNTIME_DIR=str(runtime), XDG_RUNTIME_DIR=str(runtime),
               PIPEWIRE_REMOTE='pipewire-0', DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent')
    flags = subprocess.check_output(['pkg-config','--cflags','--libs','libpipewire-0.3'], text=True)
    executable = runtime / 'meter-check'
    subprocess.run(['cc','-std=gnu11','-D_GNU_SOURCE','-O2','-Wall','-Wextra','-Werror',
                    str(ROOT/'tests/meter_lifecycle.c'),'-o',str(executable),*shlex.split(flags),'-lm'],check=True)
    with (BASE/'private-pipewire-test.log').open('w') as log:
        server = subprocess.Popen(['pipewire'],env=env,stdout=log,stderr=log)
        try:
            for _ in range(50):
                if (runtime/'pipewire-0').is_socket(): break
                if server.poll() is not None: raise RuntimeError('Private PipeWire exited')
                time.sleep(.1)
            else: raise RuntimeError('Private PipeWire did not create its socket')
            subprocess.run(['pw-cli','create-node','adapter',
                '{ factory.name = support.null-audio-sink node.name = wavelane-private-test media.class = Audio/Sink audio.position = [ FL FR ] object.linger = true }'],
                env=env,check=True,capture_output=True,text=True,timeout=5)
            subprocess.run([str(executable)],env=env,check=True,timeout=8)
        finally:
            server.terminate()
            try: server.wait(timeout=5)
            except subprocess.TimeoutExpired: server.kill(); server.wait()
