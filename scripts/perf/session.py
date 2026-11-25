import os
from pathlib import Path
import subprocess
import tempfile


REPO = Path(__file__).resolve().parents[2]


def artifact_dir(parent, prefix):
    runtime = Path(os.environ.get('XDG_RUNTIME_DIR', ''))
    bus = os.environ.get('DBUS_SESSION_BUS_ADDRESS', '').partition(',')[0]
    if (not runtime.is_absolute() or not runtime.name.startswith('sumie-rt.')
            or bus != f'unix:path={runtime}/bus' or os.environ.get('WAYLAND_DISPLAY') != 'wl-sumie'
            or 'DISPLAY' in os.environ or 'WAYLAND_SOCKET' in os.environ):
        raise RuntimeError('start with the private performance runner')
    parent = Path(parent).resolve(strict=True)
    if parent.is_relative_to(REPO):
        raise RuntimeError('artifact directory must be outside the repository')
    return Path(tempfile.mkdtemp(prefix=prefix, dir=parent))


def sumie_binary(profile):
    target = Path(os.environ.get('CARGO_TARGET_DIR', '/scratch/ssd/sumie-target-baseline'))
    return Path(os.environ.get('SUMIE_BIN', target / profile / 'sumie')).resolve()


def stop(client):
    if client.poll() is None:
        client.terminate()
        try:
            client.wait(timeout=3)
        except subprocess.TimeoutExpired:
            client.kill()
            client.wait()
