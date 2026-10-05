"""Retain notices for actual registry crates compiled in a fresh driver target.

Run after the selected release build. Registry paths come from compiler depfiles;
this includes build dependencies, without admitting unrelated workspace/models.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import tomllib

parser=argparse.ArgumentParser()
parser.add_argument('--target',type=Path,required=True)
parser.add_argument('--lock',type=Path,required=True)
parser.add_argument('--cache',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
checksums={(p['name'],p['version']):p.get('checksum') for p in tomllib.loads(args.lock.read_text())['package']}
packages={}
seen=set()
external_root=args.output/'registry-notices'
external=json.loads((external_root/'sources.json').read_text()) if (external_root/'sources.json').is_file() else {}
for dep in (args.target/'release/deps').glob('*.d'):
    for match in re.finditer(r'(/[^\s\\:]+/registry/src/[^/\s]+/[^/\s]+)',dep.read_text()):
        directory=Path(match.group(1))
        if directory in seen:continue
        seen.add(directory)
        if (directory/'Cargo.toml').is_file():
            package=tomllib.loads((directory/'Cargo.toml').read_text())['package']
            packages[(package['name'],package['version'])]=(directory,package)
assert len(packages)>50, 'Not a complete compiled driver dependency inventory'
args.output.mkdir(parents=True,exist_ok=True)
records=[];parts=[]
for (name,version),(directory,package) in sorted(packages.items()):
    license=package.get('license','')
    if not isinstance(license,str) or not license:
        raise ValueError(f'Missing license declaration: {name} {version}')
    if any(term in license for term in ('AGPL','GPL','FSL')):
        raise ValueError(f'Unexpected reciprocal dependency requires source audit: {name} {license}')
    files=[]
    for path in directory.iterdir():
        if path.is_file() and any(path.name.upper().startswith(x) for x in ('LICENSE','LICENCE','COPYING','NOTICE','COPYRIGHT','UNLICENSE')):
            files.append(path)
    if package.get('license-file'):
        path=directory/package['license-file']
        if path.is_file() and path.resolve().is_relative_to(directory.resolve()):files.append(path)
    files=sorted(set(files))
    if not files:
        # Some crates put their copyright/terms only in README.
        files=[p for p in directory.iterdir() if p.is_file() and p.name.upper().startswith('README')]
    if not files and f'{name}@{version}' in external:
        notice=external[f'{name}@{version}'];file=external_root/notice['file']
        assert hashlib.sha256(file.read_bytes()).hexdigest()==notice['sha256']
        files=[file]
    if not files:raise ValueError(f'No license/notice file: {name} {version}')
    checksum=checksums[(name,version)]
    record={'name':name,'version':version,'license':license,'crate_sha256':checksum,'source_url':f'https://crates.io/api/v1/crates/{name}/{version}/download','notices':[]}
    parts.append(f'\n===== {name} {version} ({license}) =====\nSource: {record["source_url"]}\n')
    for file in files:
        data=file.read_bytes();text=data.decode('utf-8')
        record['notices'].append({'file':file.name,'sha256':hashlib.sha256(data).hexdigest()})
        parts.append(f'\n--- {file.name} ---\n{text}\n')
    if 'MPL-2.0' in license:
        candidates=list(args.cache.glob(f'*/{name}-{version}.crate'))
        assert len(candidates)==1, f'Missing exact source archive: {name}'
        archive=candidates[0];assert hashlib.sha256(archive.read_bytes()).hexdigest()==checksum
        sources=args.output/'sources';sources.mkdir(exist_ok=True)
        shutil.copy2(archive,sources/archive.name);record['included_source']='sources/'+archive.name
    records.append(record)
(args.output/'RUST-DEPENDENCY-NOTICES.txt').write_text(''.join(parts))
(args.output/'RUST-DEPENDENCIES.json').write_text(json.dumps({'method':'Fresh selected release compiler depfiles; includes build dependencies','cargo_lock_sha256':hashlib.sha256(args.lock.read_bytes()).hexdigest(),'packages':records},indent=2)+'\n')
print(json.dumps({'packages':len(records),'included_source_archives':sum('included_source' in p for p in records)}))
