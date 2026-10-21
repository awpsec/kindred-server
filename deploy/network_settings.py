"""Durable web overrides. A changed Compose value wins for that field only."""
import json
import os
from pathlib import Path
import uuid
import ipaddress
import re
from urllib.parse import urlsplit

FIELDS = ('access', 'tailnet_address', 'public_url', 'allowed_origins', 'confirmed_http_origins')


def validate_urls(settings):
    def private(host):
        try:
            address=ipaddress.ip_address(host)
            return any(address in ipaddress.ip_network(net) for net in (
                '10.0.0.0/8','172.16.0.0/12','192.168.0.0/16','127.0.0.0/8',
                '169.254.0.0/16','100.64.0.0/10','::1/128','fc00::/7','fe80::/10'))
        except ValueError:
            return host=='localhost' or host.endswith(('.ts.net','.local'))
    for value in [settings['public_url'],*settings['allowed_origins'],*settings['confirmed_http_origins']]:
        if len(value)>2048 or any(ord(c)<32 or ord(c)==127 for c in value) or any(c in value for c in ('?','#','\\')):
            raise ValueError('Use complete HTTP or HTTPS URLs without paths or credentials')
        url=urlsplit(value)
        if url.scheme not in ('http','https') or not url.hostname or url.username is not None or url.password is not None or url.path not in ('','/'):
            raise ValueError('Use complete HTTP or HTTPS URLs without paths or credentials')
        url.port # Validate the port before saving.
        try: ipaddress.ip_address(url.hostname)
        except ValueError:
            if not re.fullmatch(r'[a-zA-Z0-9.-]+',url.hostname.encode('idna').decode()):
                raise ValueError('Invalid server hostname')
        if url.scheme=='http' and not private(url.hostname) and (value==settings['public_url'] or value not in settings['confirmed_http_origins']):
            raise ValueError('Public server URLs require HTTPS')
    if any(not value.startswith('http://') for value in settings['confirmed_http_origins']):
        raise ValueError('Confirmed HTTP URLs must use http://')


def atomic_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    try:
        with open(temporary, 'x', opener=lambda p, flags: os.open(p, flags, 0o600)) as stream:
            json.dump(value, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        temporary.unlink(missing_ok=True)


def read_state(path):
    raw = Path(path).read_bytes()
    if len(raw) > 32768:
        raise ValueError('Network settings are too large')
    value = json.loads(raw)
    if (not isinstance(value, dict) or not isinstance(value.get('revision'), str)
            or not isinstance(value.get('baseline'), dict) or not isinstance(value.get('settings'), dict)
            or set(value['baseline']) != set(FIELDS) or set(value['settings']) != set(FIELDS)):
        raise ValueError('Invalid saved network settings; restore network-settings.json from backup')
    return value


def load(root, env):
    mode = env.get('KINDRED_NETWORK_MODE', 'bridge')
    if mode not in ('bridge', 'host'):
        raise ValueError('KINDRED_NETWORK_MODE must be bridge or host')
    defaults = {
        'access': env.get('KINDRED_ACCESS', 'local') if mode == 'host' else 'bridge',
        'tailnet_address': env.get('KINDRED_TAILNET_ADDRESS', '') if mode == 'host' else '',
        'public_url': env.get('KINDRED_PUBLIC_URL', 'http://127.0.0.1:9444').rstrip('/'),
        'allowed_origins': json.loads(env.get('KINDRED_ALLOWED_ORIGINS', '[]')),
        'confirmed_http_origins': json.loads(env.get('KINDRED_CONFIRMED_HTTP_ORIGINS', '[]')),
    }
    path = Path(root) / 'network-settings.json'
    previous = read_state(path) if path.exists() else None
    settings = {key: previous['settings'][key] if previous and previous['baseline'][key] == value else value
                for key, value in defaults.items()}
    # A deliberate switch between bridge and host mode must not inherit an
    # incompatible bind policy from the previous topology.
    if mode == 'bridge':
        settings.update(access='bridge', tailnet_address='')
    elif settings['access'] == 'bridge':
        settings.update(access=defaults['access'], tailnet_address=defaults['tailnet_address'])
    for value in (defaults, settings):
        if not all(isinstance(value[key], str) for key in ('access','tailnet_address','public_url')):
            raise ValueError('Network access and URL settings must be strings')
        for key in ('allowed_origins','confirmed_http_origins'):
            if not isinstance(value[key], list) or len(value[key])>8 or not all(isinstance(item,str) for item in value[key]):
                raise ValueError('Allowed URLs must be a JSON array of at most eight addresses')
        if not set(value['confirmed_http_origins']).issubset(value['allowed_origins']):
            raise ValueError('Confirmed HTTP URLs must also be allowed URLs')
        listeners(value)
        validate_urls(value)
    revision = previous['revision'] if previous and previous['settings'] == settings else str(uuid.uuid4())
    state = {'revision': revision, 'baseline': defaults, 'settings': settings}
    atomic_json(path, state)
    descriptor = {**state, 'mode': mode, 'path': str(path)}
    atomic_json(Path(root) / 'network-applied.json', descriptor)
    return descriptor


def listeners(settings):
    access = settings['access']
    if access in ('bridge', 'all'):
        return '0.0.0.0:9444', []
    if access == 'local':
        return '127.0.0.1:9444', []
    if access == 'tailnet':
        import ipaddress
        address = ipaddress.ip_address(settings['tailnet_address'])
        if address not in ipaddress.ip_network('100.64.0.0/10') and address not in ipaddress.ip_network('fd7a:115c:a1e0::/48'):
            raise ValueError('Enter an address assigned to this Linux host by Tailscale')
        return '127.0.0.1:9444', [f'[{address}]:9444' if address.version == 6 else f'{address}:9444']
    raise ValueError('KINDRED_ACCESS must be local, tailnet or all')
