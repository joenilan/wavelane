#!/usr/bin/env python3
"""Build separately named, optimized DEV binaries; never launch or install them."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
REVIEW = ROOT / 'target/dev-review'
REVIEW.mkdir(parents=True, exist_ok=True)
# Read Cargo's actual executable path, including any CARGO_TARGET_DIR override.
# Use one source tree for both features; source snapshots need distinct targets.
builds = []
for backend, features in [('default', 'dev-shortcut'), ('fast', 'dev-shortcut,gpui-fast')]:
    source = None
    command = ['cargo', 'build', '--manifest-path', str(ROOT/'Cargo.toml'),
               '--release', '--locked', '--features', features, '--bin', 'wavelane',
               '--message-format=json-render-diagnostics']
    with (REVIEW/f'{backend}-build.log').open('w') as log:
        process = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE,
                                   stderr=log, text=True)
        assert process.stdout is not None
        for line in process.stdout:
            log.write(line)
            try: event = json.loads(line)
            except json.JSONDecodeError: continue
            if (event.get('reason') == 'compiler-artifact'
                    and event.get('target', {}).get('name') == 'wavelane'
                    and event.get('executable')):
                source = Path(event['executable']).resolve(strict=True)
        if process.wait():
            raise SystemExit(f'{backend} build failed; see {REVIEW / (backend+"-build.log")}')
    if source is None:
        raise SystemExit(f'{backend}: Cargo did not report an executable')
    destination = REVIEW/f'wavelane-DEV-{backend}'
    # Never modify an existing executable in place, including a running DEV app.
    descriptor, temp_name = tempfile.mkstemp(prefix=f'.{destination.name}-', dir=REVIEW)
    os.close(descriptor)
    temp = Path(temp_name)
    try:
        shutil.copy2(source, temp)
        version = subprocess.check_output([str(temp), '--version'], text=True).strip()
        expected = 'Wavelane DEV ' + ('Fast' if backend == 'fast' else 'Default')
        if not version.startswith(expected+' '):
            raise SystemExit(f'Unexpected backend identity: {version!r}')
        temp.replace(destination)
    finally:
        temp.unlink(missing_ok=True)
    with destination.open('rb') as file:
        digest = hashlib.file_digest(file, 'sha256').hexdigest()
    builds.append(dict(backend=backend, features=features, profile='release',
                       executable=str(destination), sha256=digest, version=version))
    print(f'{version}: {destination}', flush=True)
assert builds[0]['sha256'] != builds[1]['sha256'], 'Backend builds must differ'
manifest = dict(builds=builds,
                source_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
                source_dirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT)),
                lockfile_sha256=hashlib.file_digest((ROOT/'Cargo.lock').open('rb'),'sha256').hexdigest(),
                synthetic_harness=False, launched=False, installed=False)
(REVIEW/'backend-builds.json').write_text(json.dumps(manifest,indent=2)+'\n')
# Preserve the previous review entry point as the default backend.
legacy = REVIEW/'wavelane-DEV'
descriptor, temp_name = tempfile.mkstemp(prefix='.wavelane-DEV-', dir=REVIEW)
os.close(descriptor)
temp = Path(temp_name)
try:
    shutil.copy2(REVIEW/'wavelane-DEV-default', temp)
    temp.replace(legacy)
finally:
    temp.unlink(missing_ok=True)
print('Both DEV backends are ready. Quit the current DEV app before switching.', flush=True)
