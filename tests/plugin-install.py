#!/usr/bin/env python3
"""Install into an isolated Codex home, then exercise the real host's MCP startup."""
import json
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import tempfile
import time

repo = Path(__file__).resolve().parent.parent
binary = Path(sys.argv[1]).resolve()
version = (repo / 'plugins/hwahap/version.txt').read_text().strip()
with tempfile.TemporaryDirectory(prefix='hwahap-install-') as directory:
    base = Path(directory)
    market = base / 'marketplace'
    plugin = market / 'plugins/hwahap'
    shutil.copytree(repo / 'plugins/hwahap', plugin, ignore=shutil.ignore_patterns('target'))
    shutil.copytree(repo / '.agents', market / '.agents')
    release = plugin / 'runtime/target/release/hwahap'
    release.parent.mkdir(parents=True)
    shutil.copy2(binary, release)
    home = base / 'codex-home'
    home.mkdir()
    env = dict(os.environ, CODEX_HOME=str(home), HWAHAP_OFFLINE='1')
    def cli(*args):
        result = subprocess.run(['codex', *args], env=env, text=True, capture_output=True, timeout=30)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)
    cli('plugin', 'marketplace', 'add', str(market), '--json')
    installed = cli('plugin', 'add', 'hwahap@hwahap', '--json')
    assert installed['version'] == version
    servers = cli('mcp', 'list', '--json')
    assert servers[0]['transport']['cwd'].startswith(installed['installedPath'])
    with (base / 'stderr.log').open('w') as stderr:
        process = subprocess.Popen(['codex', 'app-server', '--stdio'], env=env,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr)
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        pending = b''
        def request(identifier, method, params):
            global pending
            message = {'method': method, 'params': params}
            if identifier is not None:
                message['id'] = identifier
            process.stdin.write((json.dumps(message) + '\n').encode())
            process.stdin.flush()
            if identifier is None:
                return
            deadline = time.monotonic() + 45
            while time.monotonic() < deadline:
                while b'\n' in pending:
                    line, pending = pending.split(b'\n', 1)
                    reply = json.loads(line)
                    if reply.get('id') == identifier:
                        assert 'error' not in reply, reply
                        return reply['result']
                assert selector.select(max(0, deadline - time.monotonic())), 'Codex response timeout'
                data = os.read(process.stdout.fileno(), 65536)
                assert data, 'Codex exited before responding'
                pending += data
            raise AssertionError('Codex response timeout')
        try:
            request(1, 'initialize', {'clientInfo': {'name': 'hwahap-install-test', 'version': version},
                                     'capabilities': {'experimentalApi': True}})
            request(None, 'initialized', {})
            thread = request(2, 'thread/start', {'cwd': str(base), 'ephemeral': True})
            status = request(3, 'mcpServerStatus/list', {'threadId': thread['thread']['id']})
            server = next(s for s in status['data'] if s['name'] == 'hwahap')
            assert server['runtimeStatus'] == 'connected', server
            assert set(server['tools']) == {'hwahap_step', 'hwahap_status', 'hwahap_ship'}, server
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            selector.close()
    print(f'Codex plugin {version}: isolated install and real MCP startup passed')
