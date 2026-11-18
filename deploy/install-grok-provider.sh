#!/bin/sh
# Install the optional official xAI CLI inside a Linux x64 bot VM.
set -eu
[ "$(uname -sm)" = "Linux x86_64" ]
provider_dir=/opt/kindred/providers
install -d -m 755 "$provider_dir"
install_stage=$(mktemp -d "$provider_dir/.grok-stage-XXXXXXXX")
trap 'rm -rf "$install_stage"' EXIT
python3 - "$install_stage" <<'PY'
import base64,hashlib,io,sys,tarfile,urllib.request
from pathlib import Path
dest=Path(sys.argv[1])
url='https://registry.npmjs.org/@xai-official/grok-linux-x64/-/grok-linux-x64-1.0.88.tgz'
with urllib.request.urlopen(url, timeout=120) as response:
    data=response.read(70_000_000)
expected='ggXRZTrxVlBAta4Y1G3vJ7oXp/8wXGsgWlKeBn9SwkA4F30B721xDjhkDGAerJFA9ZABE+sR7dhlHfoFhEMx0Q=='
if hashlib.sha512(data).digest()!=base64.b64decode(expected):
    sys.exit('Unverified Grok package')
with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
    member=archive.getmember('package/bin/grok.br')
    if not member.isfile():sys.exit('Invalid Grok binary')
    (dest/'grok.br').write_bytes(archive.extractfile(member).read())
PY
node - "$install_stage" <<'JS'
const fs = require('node:fs');
const zlib = require('node:zlib');
const { pipeline } = require('node:stream/promises');
const stage = process.argv[2];
pipeline(fs.createReadStream(stage+'/grok.br'), zlib.createBrotliDecompress(),
         fs.createWriteStream(stage+'/grok', {mode: 0o755, flags:'wx'}))
    .catch(error => { console.error(error.message); process.exitCode=1; });
JS
"$install_stage/grok" --version | grep -Fx 'grok 1.0.88 (2fe3608f7398)'
mv -f "$install_stage/grok" "$provider_dir/grok-1.0.88"
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install -D -m 755 "$script_dir/grok-account.py" /usr/local/lib/kindred/grok-account.py
