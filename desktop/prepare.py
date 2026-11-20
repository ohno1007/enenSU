"""Prepare the pinned Windows resources for a single enenSU desktop executable."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
from urllib.request import urlopen
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED

DESKTOP = Path(__file__).resolve().parent
DIST = DESKTOP / 'dist'
SDK = ('https://dl.google.com/android/repository/platform-tools_r37.0.3-win.zip',
       '89d108360a3bf2924d3bea837239edaccc22fd6ff5befd3787c4348a9fe4f09d')
WEBVIEW = ('https://msedge.sf.dl.delivery.mp.microsoft.com/filestreamingservice/files/068be74d-a831-494f-8f57-c173756d5a33/MicrosoftEdgeWebview2Setup.exe',
           'cf2f2445fef0c5c14e372045f84998771eeb8bb01d59be4afd00692fa160082e')


def verified(source, local):
    url, expected = source
    if local:
        data = local.read_bytes()
    else:
        with urlopen(url, timeout=90) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError(f'Checksum mismatch for {url}')
    return data


def notices(target):
    ui = DESKTOP / 'ui' / 'node_modules'
    directories = [(package, ui / package) for package in ['@fontsource/roboto', '@material-design-icons/svg', '@material/web', 'lit', 'lit-html', 'lit-element', '@lit/reactive-element', 'tslib']]
    metadata = subprocess.check_output(['cargo', 'metadata', '--format-version=1', '--locked', '--features', 'desktop', '--filter-platform', target], cwd=DESKTOP.parent)
    for pkg in json.loads(metadata)['packages']:
        directories.append((f"{pkg['name']} {pkg['version']} ({pkg['license'] or 'see source'})", Path(pkg['manifest_path']).parent))
    blocks = []
    for name, directory in directories:
        files = sorted(p for p in directory.iterdir() if p.is_file() and p.name.lower().startswith(('license', 'copying', 'notice')))
        contents = [p.read_text(encoding='utf-8', errors='replace') for p in files]
        if name.startswith('tinyfiledialogs '):
            contents.append((directory / 'libtinyfiledialogs/tinyfiledialogs.c').read_text(errors='replace').split('*/', 1)[0] + '*/')
        blocks.append('\n'.join([name, *contents]))
    return '\n\n'.join(blocks).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform-archive', type=Path)
    parser.add_argument('--webview-bootstrapper', type=Path)
    parser.add_argument('--target', default='x86_64-pc-windows-msvc')
    args = parser.parse_args()
    DIST.mkdir(exist_ok=True)
    npm = shutil.which('npm.cmd') or shutil.which('npm')
    subprocess.run([npm, 'ci', '--no-audit', '--no-fund'], cwd=DESKTOP / 'ui', check=True)
    subprocess.run([npm, 'run', 'build'], cwd=DESKTOP / 'ui', check=True)
    (DIST / 'webview-setup.exe').write_bytes(verified(WEBVIEW, args.webview_bootstrapper))
    with ZipFile(io.BytesIO(verified(SDK, args.platform_archive))) as source:
        files = {name: source.read('platform-tools/' + name) for name in [
            'adb.exe', 'fastboot.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll', 'NOTICE.txt', 'source.properties']}
    files['THIRD-PARTY-NOTICES.txt'] = notices(args.target)
    files['LICENSE.txt'] = (DESKTOP.parent / 'LICENSE').read_bytes()
    with ZipFile(DIST / 'tools.zip', 'w') as archive:
        for name, data in files.items():
            archive.writestr(ZipInfo(name), data, compress_type=ZIP_DEFLATED, compresslevel=9)


if __name__ == '__main__':
    main()
