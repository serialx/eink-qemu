#!/usr/bin/env python3
"""Prepare static font downloads; never runs as a proxy or runtime server."""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import tempfile
import urllib.request
import zlib

CATALOG = 'https://github.com/crosspoint-reader/crosspoint-fonts/releases/download/sd-fonts-m1-b4/fonts.json'
DEFAULT_FAMILIES = ['Alef', 'Literata', 'NotoSansExtended', 'Pretendard']
GOOGLE = 'https://raw.githubusercontent.com/google/fonts/main/ofl/'
LICENSES = {name: GOOGLE + name.lower() + '/OFL.txt' for name in [
    'Alef', 'Alegreya', 'Amiri', 'AtkinsonHyperlegibleNext', 'Bitter', 'DavidLibre',
    'FrankRuhlLibre', 'GentiumBookPlus', 'IBMPlexMono', 'IBMPlexSans', 'IBMPlexSerif',
    'Inter', 'LibreBaskerville', 'Literata', 'Lora', 'Merriweather', 'NotoKufiArabic',
    'SourceCodePro', 'SourceSans3', 'SourceSerif4', 'Vollkorn']}
LICENSES.update({
    'NotoSansExtended': 'https://raw.githubusercontent.com/google/fonts/main/ofl/notosans/OFL.txt',
    'NotoSerifExtended': 'https://raw.githubusercontent.com/google/fonts/main/ofl/notoserif/OFL.txt',
    'Pretendard': 'https://raw.githubusercontent.com/orioncactus/pretendard/v1.3.9/LICENSE',
    'PretendardMedium': 'https://raw.githubusercontent.com/orioncactus/pretendard/v1.3.9/LICENSE',
})

def download(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read()


def valid_font(data, entry):
    return len(data) == entry['size'] and zlib.crc32(data) == entry['crc32']


def prepare(destination, catalog_url=CATALOG, families=None, license_dir=None):
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    catalog = json.loads(download(catalog_url))
    selected = set(families or DEFAULT_FAMILIES)
    available = {family['name'] for family in catalog['families']}
    if selected - available:
        raise ValueError(f'Unknown font families: {sorted(selected - available)}')
    catalog['families'] = [family for family in catalog['families'] if family['name'] in selected]
    routes = {}
    sources = {}
    jobs = []
    for family in catalog['families']:
        name = family['name']
        if not re.fullmatch(r'[A-Za-z0-9_-]+', name):
            raise ValueError('Unsafe font family name')
        license_path = destination / 'licenses' / (name + '.txt')
        license_path.parent.mkdir(exist_ok=True)
        local_license = Path(license_dir) / (name + '.txt') if license_dir else None
        if local_license and local_license.is_file():
            license_path.write_bytes(local_license.read_bytes())
        elif name in LICENSES:
            if not license_path.exists():
                license_path.write_bytes(download(LICENSES[name]))
        else:
            raise ValueError(f'Provide {name}.txt in --license-dir for this font family')
        for entry in family['files']:
            filename = entry['name']
            if not re.fullmatch(r'[A-Za-z0-9_.-]+\.cpfont', filename):
                raise ValueError('Unsafe font filename')
            url = catalog['baseUrl'] + filename
            if not url.startswith('https://'):
                raise ValueError('Font source must use HTTPS')
            routes[url] = filename
            jobs.append((url, entry))

    def copy_font(job):
        url, entry = job
        path = destination / entry['name']
        data = path.read_bytes() if path.exists() else b''
        if not valid_font(data, entry):
            data = download(url)
            if not valid_font(data, entry):
                raise ValueError(f'Font size/CRC mismatch: {entry["name"]}')
            temporary = path.with_suffix('.part')
            temporary.write_bytes(data)
            temporary.replace(path)
        return entry['name'], {'url': url, 'sha256': hashlib.sha256(data).hexdigest()}

    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
        for name, source in pool.map(copy_font, jobs):
            sources[name] = source
    # Keep upstream URLs in the manifest. The browser maps these exact URLs to
    # local static files, so no firmware patch or redirect is necessary.
    manifest_name = 'fonts.json'
    (destination / manifest_name).write_text(json.dumps(catalog, ensure_ascii=False), encoding='utf8')
    routes[catalog_url] = manifest_name
    index = {'catalog': catalog_url, 'urls': routes, 'sources': sources}
    temporary = destination / 'index.json.part'
    temporary.write_text(json.dumps(index, indent=2) + '\n')
    temporary.replace(destination / 'index.json')
    print(f'Prepared {len(jobs)} fonts in {destination} ({", ".join(sorted(selected))})')


def copy_bundle(source, destination):
    """Publish only selected assets, retaining the old bundle on validation failure."""
    source, destination = Path(source).resolve(), Path(destination).resolve()
    if source == destination or source in destination.parents or destination in source.parents:
        raise ValueError('Source and package directories must not overlap')
    index = json.loads((source / 'index.json').read_text())
    catalog = json.loads((source / 'fonts.json').read_text())
    fonts = {}
    notices = set()
    for family in catalog['families']:
        if not re.fullmatch(r'[A-Za-z0-9_-]+', family['name']):
            raise ValueError('Unsafe font family name')
        notices.add('licenses/' + family['name'] + '.txt')
        for entry in family['files']:
            name = entry['name']
            if not re.fullmatch(r'[A-Za-z0-9_.-]+\.cpfont', name):
                raise ValueError('Unsafe font filename')
            fonts[name] = entry
    if (set(index['sources']) != set(fonts) or
            set(index['urls'].values()) != set(fonts) | {'fonts.json'} or
            index['urls'].get(index['catalog']) != 'fonts.json'):
        raise ValueError('Asset index does not match the selected catalog')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.wifi-assets-', dir=destination.parent) as temporary:
        staging = Path(temporary) / 'bundle'
        staging.mkdir()
        for name in ['index.json', 'fonts.json', *sorted(notices), *fonts]:
            data = (source / name).read_bytes()
            if name in fonts and (not valid_font(data, fonts[name]) or
                    hashlib.sha256(data).hexdigest() != index['sources'][name]['sha256']):
                raise ValueError(f'Font integrity check failed: {name}')
            target = staging / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        previous = Path(temporary) / 'previous'
        if destination.exists():
            destination.rename(previous)
        try:
            staging.rename(destination)
        except OSError:
            if previous.exists():
                previous.rename(destination)
            raise
    print(f'Packaged {len(fonts)} selected fonts in {destination}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    parser.add_argument('--catalog', default=CATALOG)
    parser.add_argument('--family', action='append', help='Repeat to select families; defaults to Alef, Literata, NotoSansExtended, Pretendard')
    parser.add_argument('--license-dir', type=Path, help='Directory containing <Family>.txt license notices for other families')
    parser.add_argument('--copy-to', type=Path, help='Copy an existing prepared bundle here without downloading; omit unselected cached files')
    args = parser.parse_args()
    if args.copy_to:
        if args.family or args.license_dir or args.catalog != CATALOG:
            parser.error('--copy-to uses the prepared catalog; do not combine it with preparation options')
        copy_bundle(args.destination, args.copy_to)
    else:
        prepare(args.destination, args.catalog, args.family, args.license_dir)


if __name__ == '__main__':
    main()
