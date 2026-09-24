#!/bin/sh
# Run as root inside the bot VM. Official CLIs own their subscription credentials.
set -eu
if ! python3 -c "import ensurepip" 2>/dev/null; then
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq python3-venv
fi
case "$(uname -m)" in x86_64) ;; *) echo "This pinned provider installer currently supports Linux x86_64." >&2; exit 1;; esac
root=/opt/kindred/providers
install -d -m 755 "$root"
python3 - "$root" <<'PY'
import base64, hashlib, io, json, pathlib, sys, tarfile, urllib.request
root=pathlib.Path(sys.argv[1]); version='2.1.263'
dest=root/('claude-'+version)
if not dest.exists():
    metadata=json.load(urllib.request.urlopen('https://registry.npmjs.org/@anthropic-ai/claude-code-linux-x64/'+version, timeout=60))
    dist=metadata['dist']; data=urllib.request.urlopen(dist['tarball'],timeout=120).read()
    assert 'sha512-'+base64.b64encode(hashlib.sha512(data).digest()).decode()==dist['integrity']
    with tarfile.open(fileobj=io.BytesIO(data),mode='r:gz') as archive:
        candidates=[m for m in archive.getmembers() if m.isfile() and m.name.endswith('/claude')]
        assert len(candidates)==1
        dest.write_bytes(archive.extractfile(candidates[0]).read()); dest.chmod(0o755)
PY
if [ ! -x "$root/kimi-1.50.0/bin/kimi" ]; then
    python3 -m venv "$root/kimi-1.50.0"
    "$root/kimi-1.50.0/bin/python" -m pip install --disable-pip-version-check 'kimi-cli==1.50.0'
fi
"$root/claude-2.1.263" --version
"$root/kimi-1.50.0/bin/kimi" --version

source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install -d -m 755 /usr/local/lib/kindred
install -m 755 "$source/provider-cli.py" /usr/local/lib/kindred/provider-cli.py
install -m 755 "$source/provider-login.py" /usr/local/lib/kindred/provider-login.py
install -m 644 "$source/provider-connectors.py" /usr/local/lib/kindred/provider-connectors.py
