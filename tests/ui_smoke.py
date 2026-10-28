"""Offline browser smoke test: Python stdlib + Firefox, no Node dependencies.

Run inside an isolated loopback sandbox; all APIs below are dummy fixtures.
"""
import base64
import functools
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

PUBLIC = Path(__file__).resolve().parents[1] / 'druid-garden-os-ui' / 'public'
requests = []
authenticated = False


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path.split('?')[0] in ['/', '/index.html', '/app.js', '/ui.js', '/styles.css']:
            return super().do_GET()
        self.api()

    def do_POST(self):
        self.api()

    do_PUT = do_POST
    do_DELETE = do_POST

    def api(self):
        global authenticated
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', '0'))) or b'null')
        requests.append((self.command, self.path, body))
        status, result = 200, True
        if self.path == '/auth/login':
            assert body == {'username': 'Admin', 'password': 'dummy-password'}
            authenticated = True
        elif self.path == '/auth/logout':
            authenticated = False
        elif self.path == '/api/users/me':
            if not authenticated:
                status, result = 401, 'Sign in'
            else:
                result = {'id': 1, 'username': 'Admin', 'role': 'SuperAdmin', 'expires_at': int(time.time()) + 1800, 'must_change_password': False}
        elif self.path == '/api/users':
            result = [{'id': 1, 'username': 'Admin', 'role': 'SuperAdmin'}]
        elif self.path == '/auth/jwt':
            if not authenticated:
                status, result = 401, 'Sign in'
            else:
                claims = {'sub': '1', 'eml': 'Admin', 'rol': 'SuperAdmin', 'exp': int(time.time()) + 1800}
                part = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip('=')
                result = 'fixture.' + part + '.fixture'
        elif self.path.startswith('/api/users/password/'):
            result = False
        elif self.path == '/api/system/info':
            result = {'hostname': 'Test garden', 'arch': 'x86_64', 'os_version': 'Fixture OS', 'kernel': 'test', 'uptime': 120, 'running_processes': []}
        elif self.path == '/api/system/cpu':
            result = {'global_usage': 12.5, 'physical_count': 4, 'thread_count': 8}
        elif self.path == '/api/system/memory':
            result = {'used': 1024, 'total': 4096}
        elif self.path == '/api/system/heartbeat/health':
            result = {'device_id': 'fixture-device', 'firmware_version': '1.1.1', 'last_success_unix': None, 'consecutive_failures': 0}
        elif self.path.startswith('/config/'):
            result = None
        elif self.path == '/led/brightness':
            result = 128
        elif self.path in ['/farmer/status', '/farmer/state', '/farmer/config']:
            result = {}
        elif self.path in ['/api/system/networks', '/api/system/gpus', '/api/system/disks', '/api/files/list', '/farmer/stats', '/api/plugins', '/api/plugins/available', '/config']:
            result = []
        elif self.path == '/system/hotspot/active':
            result = False
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(json.dumps(result).encode())


class Marionette:
    def __init__(self, port):
        self.socket = socket.create_connection(('127.0.0.1', port), timeout=30)
        self.sequence = 0
        self.read()

    def read(self):
        size = b''
        while not size.endswith(b':'):
            chunk = self.socket.recv(1)
            if not chunk:
                raise RuntimeError('Browser disconnected')
            size += chunk
        remaining = int(size[:-1])
        result = b''
        while remaining:
            chunk = self.socket.recv(remaining)
            if not chunk:
                raise RuntimeError('Browser disconnected')
            result += chunk
            remaining -= len(chunk)
        return json.loads(result)

    def call(self, name, params=None):
        self.sequence += 1
        payload = json.dumps([0, self.sequence, name, params or {}]).encode()
        self.socket.sendall(str(len(payload)).encode() + b':' + payload)
        reply = self.read()
        if reply[2]:
            raise RuntimeError(reply[2])
        return reply[3]

    def script(self, source):
        result = self.call('WebDriver:ExecuteScript', {'script': source, 'args': [], 'newSandbox': True})
        return result.get('value') if isinstance(result, dict) else result

    def wait(self, source):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.script(source):
                return
            time.sleep(0.05)
        raise AssertionError('Timed out: ' + source)


def main():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(PUBLIC)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='garden-browser-') as scratch:
        profile = Path(scratch) / 'profile'
        profile.mkdir()
        port = 2828
        prefs = {'marionette.port': port, 'browser.shell.checkDefaultBrowser': False,
                 'marionette.log.level': 'Trace',
                 'remote.log.level': 'Trace',
                 'javascript.options.wasm': False,
                 'browser.startup.page': 0, 'browser.startup.homepage': 'about:blank',
                 'browser.startup.firstrunSkipsHomepage': True,
                 'startup.homepage_welcome_url': '', 'startup.homepage_welcome_url.additional': '',
                 'datareporting.policy.firstRunURL': '', 'browser.aboutwelcome.enabled': False}
        (profile / 'user.js').write_text(''.join(f'user_pref({json.dumps(key)}, {json.dumps(value)});\n' for key, value in prefs.items()))
        with open(Path(scratch) / 'firefox.log', 'w+') as log:
            process = subprocess.Popen(['firefox', '--headless', '--no-remote', '--marionette', '--profile', str(profile), 'about:blank'], stdout=log, stderr=log, env={**os.environ, 'MOZ_DISABLE_CONTENT_SANDBOX': '1'})
            try:
                browser = None
                for _ in range(200):
                    try:
                        browser = Marionette(port)
                        break
                    except (ConnectionRefusedError, TimeoutError):
                        if process.poll() is not None:
                            log.seek(0)
                            raise RuntimeError(log.read()[-3000:])
                        time.sleep(0.1)
                assert browser, 'Firefox did not start'
                browser.call('WebDriver:NewSession', {'capabilities': {'alwaysMatch': {}}})
                browser.call('WebDriver:Navigate', {'url': f'http://127.0.0.1:{server.server_port}/'})
                browser.wait("return !!document.querySelector('input[name=username]')")
                browser.script("document.querySelector('[name=username]').value='Admin';document.querySelector('[name=password]').value='dummy-password';document.querySelector('form').requestSubmit()")
                browser.wait("return document.querySelector('h1')?.textContent === 'Overview'")
                browser.wait("return document.body.textContent.includes('Test garden')")
                for route, heading in [('devices', 'Device discovery'), ('farmer', 'Farmer'), ('storage', 'Storage'), ('files', 'Files'), ('network', 'Network'), ('plugins', 'Plugins'), ('leds', 'LED controls'), ('settings', 'Settings'), ('account', 'Account')]:
                    browser.script('location.hash=' + json.dumps('#' + route))
                    browser.wait('return document.querySelector("h1")?.textContent === ' + json.dumps(heading))
                    time.sleep(0.2)
                    errors = browser.script("return [...document.querySelectorAll('main .error')].map(n=>n.textContent)")
                    assert not errors, (route, errors)
                browser.script("const f=[...document.forms].find(f=>f.querySelector('[name=username]'));f.querySelector('[name=username]').value='fixture';f.querySelector('[name=password]').value='test-password-ü';f.requestSubmit()")
                browser.wait("return document.querySelector('#notice').textContent.includes('User created')")
                registration = next(body for method, path, body in requests if path == '/api/users/register')
                assert registration['password'] == list('test-password-ü'.encode())
                browser.script("[...document.querySelectorAll('button')].find(b=>b.textContent==='Sign out').click()")
                browser.wait("return !!document.querySelector('input[name=username]')")
                assert not authenticated
                assert any(path == '/auth/logout' and method == 'POST' for method, path, _ in requests)
                print('PASS: login, 10 pages, UTF-8 registration payload, server logout; no Node')
                browser.call('Marionette:Quit', {'flags': ['eForceQuit']})
            except Exception:
                log.flush()
                log.seek(0)
                print(log.read()[-1000:])
                if browser:
                    try: print(browser.script('return document.body.innerText'))
                    except Exception: pass
                raise
            finally:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    server.shutdown()


if __name__ == '__main__':
    main()
