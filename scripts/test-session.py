#!/usr/bin/env python3
"""Integration tests in private D-Bus/PipeWire instances; no physical device monitors.

Requires pipewire, pw-cli/pw-link/pw-dump/pw-cat, dbus-run-session, Python 3.
Optional GTK smoke test uses gtk4-broadwayd (no desktop session needed).
"""
import array
from concurrent.futures import ThreadPoolExecutor
import json
import math
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import wave


def parse_live_controls(props):
    """Read numeric SPA values across PipeWire debug-output versions."""
    actual = {}
    pattern = r'String "([^"\n]+)"\n\s+(Float|Double|Int|Long|Bool) ([^\n]+)'
    for key, kind, value in re.findall(pattern, props):
        value = value.strip()
        if kind == 'Bool':
            if value not in ('true', 'false', '0', '1'):
                raise ValueError('Unknown SPA boolean representation')
            actual[key] = float(value in ('true', '1'))
        else:
            actual[key] = float(value)
    return actual


def main(target):
    root = Path(__file__).resolve().parents[1]
    children = []
    files = []
    with tempfile.TemporaryDirectory(prefix='bigsound-session-') as temporary:
        tmp = Path(temporary)
        runtime = tmp / 'runtime'
        runtime.mkdir(mode=0o700)
        config = tmp / 'config'
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), PIPEWIRE_RUNTIME_DIR=str(runtime),
                   XDG_CONFIG_HOME=str(config), PULSE_SERVER='unix:' + str(runtime / 'unavailable-pulse'),
                   LC_ALL='C', GSETTINGS_BACKEND='memory', GTK_A11Y='none', GTK_USE_PORTAL='0',
                   GIO_USE_VFS='local', DBUS_SYSTEM_BUS_ADDRESS='unix:path=' + str(runtime / 'unavailable-system-bus'))
        profiles = config / 'bigsound/profiles'
        profiles.mkdir(parents=True)
        for profile in (root / 'crates/daemon/data/profiles').glob('*.json'):
            shutil.copy(profile, profiles)
        dropin = config / 'pipewire/filter-chain.conf.d'
        dropin.mkdir(parents=True)
        text = (root / 'configs/pipewire/10-bigsound.conf.template').read_text()
        for name in ('bass', 'clarity', 'space', 'cross', 'loud'):
            text = text.replace('__BIG_' + name.upper() + '_SO_PATH__', str(target / f'libbig_{name}.so'))
        (dropin / '99-bigsound.conf').write_text(text)

        # Inject one second-batch failure, then delegate every other command
        # to the real tool. All state belongs to this private test fixture.
        real_pw_cli = shutil.which('pw-cli')
        assert real_pw_cli
        tools = tmp / 'tools'
        tools.mkdir()
        fail_batch = tmp / 'fail-second-batch'
        wrapper = tools / 'pw-cli'
        wrapper.write_text('#!' + sys.executable + '\n' +
            'import os,sys\nfrom pathlib import Path\n' +
            'flag=Path(' + repr(str(fail_batch)) + ')\n' +
            "if len(sys.argv)>4 and sys.argv[1]=='set-param' and sys.argv[3]=='Props' and flag.exists() and '\"bigspace:width\"' in sys.argv[4] and '\"bigbass_l:' not in sys.argv[4]:\n" +
            "    flag.unlink()\n    sys.stderr.write('Error: injected second-batch failure\\n')\n    sys.exit(1)\n" +
            'os.execv(' + repr(real_pw_cli) + ',[' + repr(real_pw_cli) + ']+sys.argv[1:])\n')
        wrapper.chmod(0o755)
        env['PATH'] = str(tools) + os.pathsep + env.get('PATH', os.defpath)

        def start(args, name, **kwargs):
            log = open(tmp / (name + '.log'), 'wb')
            files.append(log)
            proc = subprocess.Popen(args, env=env, stdout=log, stderr=log, **kwargs)
            children.append(proc)
            return proc

        def run(args, ok=True, timeout=15):
            result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=timeout)
            if ok and result.returncode:
                raise AssertionError(f'{args[0]} failed: {result.stderr[-1500:]}')
            if not ok:
                assert result.returncode != 0, args
            return result.stdout.strip()

        def cli(*args, **kwargs):
            return run([str(target / 'bigsound'), *args], **kwargs)

        def graph():
            try:
                return json.loads(run(['pw-dump'], timeout=3))
            except (AssertionError, subprocess.TimeoutExpired):
                return []

        def node(name):
            for item in graph():
                if item.get('type') == 'PipeWire:Interface:Node' and item.get('info', {}).get('props', {}).get('node.name') == name:
                    return item['id']
            return None

        def wait_for(predicate, what):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                result = predicate()
                if result:
                    return result
                time.sleep(.05)
            raise AssertionError('timed out: ' + what)

        try:
            # The PipeWire core does not discover hardware. No WirePlumber is started.
            start(['pipewire'], 'pipewire')
            wait_for(lambda: (runtime / 'pipewire-0').exists(), 'private PipeWire socket')
            chain = start(['pipewire', '-c', 'filter-chain.conf'], 'chain')
            old_id = wait_for(lambda: node('BigSound'), 'full LADSPA chain')
            daemon = start([str(target / 'bigsound-daemon')], 'daemon')
            wait_for(lambda: subprocess.run([str(target / 'bigsound'), 'list'], env=env, capture_output=True).returncode == 0, 'D-Bus daemon')
            assert len(cli('list').splitlines()) == 17
            assert len(cli('profile', 'list').splitlines()) == 15
            # Drive the REAL graph and capture actual processed samples with manual links.
            stimulus = array.array('h', (int(26000 * math.sin(i // 2 * 2 * math.pi * 220 / 48000)) for i in range(48000 * 2 * 60)))
            input_file = tmp / 'input.wav'
            with wave.open(str(input_file), 'wb') as wav:
                wav.setnchannels(2)
                wav.setsampwidth(2)
                wav.setframerate(48000)
                wav.writeframes(stimulus.tobytes())
            capture_file = tmp / 'capture.raw'
            record_output = open(capture_file, 'wb')
            files += [record_output]
            source = start(['pw-cat', '--playback', '--target', '0', '--rate', '48000', '--channels', '2', '--format', 'f32', '--properties', '{ node.name = qa-source }', str(input_file)], 'source')
            record_log = open(tmp / 'record.log', 'wb')
            files.append(record_log)
            raw_flag = ['--raw'] if '--raw' in run(['pw-cat', '--help']) else []
            recorder = subprocess.Popen(['pw-cat', *raw_flag, '--record', '--target', '0', '--rate', '48000', '--channels', '2', '--format', 'f32', '--properties', '{ node.name = qa-record }', '-'], env=env, stdout=record_output, stderr=record_log)
            children.append(recorder)
            wait_for(lambda: node('qa-source') and node('qa-record'), 'audio clients')
            for name, direction in [('BigSound', 'Input'), ('BigSound.output', 'Output'), ('qa-source', 'Output'), ('qa-record', 'Input')]:
                # A session manager normally negotiates adapter ports. Configure
                # them explicitly here so the test never needs hardware policy.
                config_pod = '{ direction = "' + direction + '" mode = "dsp" format = { mediaType = "audio" mediaSubtype = "raw" format = "F32P" rate = 48000 channels = 2 position = [ FL FR ] } }'
                result = run(['pw-cli', 'set-param', str(node(name)), 'PortConfig', config_pod])
                assert 'Error:' not in result, result
            for channel in ('FL', 'FR'):
                run(['pw-link', f'BigSound.output:output_{channel}', f'qa-record:input_{channel}'])
                run(['pw-link', f'qa-source:output_{channel}', f'BigSound:playback_{channel}'])

            cli('set', 'bigbass:mix', '0.25')
            assert float(cli('get', 'bigbass:mix')) == .25
            wait_for(lambda: '0.250000' in run(['pw-cli', 'enum-params', str(old_id), 'Props']), 'live control update')
            props = run(['pw-cli', 'enum-params', str(old_id), 'Props'])
            assert 'bigbass_l:mix' in props and 'bigbass_r:mix' in props
            # Validate the controls really changed, not merely the daemon cache.
            lines = props.splitlines()
            for name in ('bigbass_l:mix', 'bigbass_r:mix'):
                index = next(i for i, line in enumerate(lines) if name in line)
                assert '0.250000' in lines[index + 1], props
            for value in ('nan', 'inf', '-0.1', '1.1'):
                cli('set', 'bigbass:mix', value, ok=False)
            for name in ('../private', 'a/b', ' ', 'x' * 65):
                cli('profile', 'save', name, ok=False)
            def profile_is_live(params):
                props = run(['pw-cli', 'enum-params', str(old_id), 'Props'])
                actual = parse_live_controls(props)
                for key, expected in params.items():
                    module, control = key.split(':')
                    names = [module + suffix + ':' + control for suffix in ('_l', '_r')] if module in ('bigbass', 'bigclarity') else [key]
                    for internal in names:
                        if internal not in actual:
                            return False
                        value = actual[internal]
                        if not math.isfinite(value) or abs(value - expected) > 1e-5:
                            return False
                return True
            for name in cli('profile', 'list').splitlines():
                cli('profile', 'apply', name)
                params = json.loads(cli('profile', 'show', name))['params']
                wait_for(lambda: profile_is_live(params), 'all live controls for profile ' + name)
            cli('profile', 'save', 'QA Mix')
            assert (profiles / '99-user-QA Mix.json').is_file()
            cli('profile', 'delete', 'QA Mix')
            assert not (profiles / '99-user-QA Mix.json').exists()
            with ThreadPoolExecutor(max_workers=8) as pool:
                list(pool.map(lambda i: cli('set', 'bigbass:mix', str(i / 100)), range(40)))
            cli('set', 'bigbass:mix', '0.31')
            assert float(cli('get', 'bigbass:mix')) == .31
            previous = dict(params)
            previous['bigbass:mix'] = .31
            previous_profile = cli('profile', 'active')
            fail_batch.write_text('fail once')
            cli('profile', 'apply', 'BigSound', ok=False)
            assert not fail_batch.exists(), 'The second batch must have been attempted'
            assert cli('profile', 'active') == previous_profile
            assert float(cli('get', 'bigbass:mix')) == .31
            wait_for(lambda: profile_is_live(previous), 'live rollback after second profile batch failed')
            chain.terminate()
            chain.wait(timeout=5)
            wait_for(lambda: not node('BigSound'), 'filter-chain removal')
            active = cli('profile', 'active')
            cli('set', 'bigbass:mix', '0.73', ok=False)
            assert float(cli('get', 'bigbass:mix')) == .31
            cli('profile', 'apply', 'BigSound', ok=False)
            assert float(cli('get', 'bigbass:mix')) == .31
            assert cli('profile', 'active') == active
            chain = start(['pipewire', '-c', 'filter-chain.conf'], 'chain-restarted')
            fresh_id = wait_for(lambda: node('BigSound'), 'filter-chain restart')
            for name, direction in [('BigSound', 'Input'), ('BigSound.output', 'Output'), ('qa-source', 'Output'), ('qa-record', 'Input')]:
                # A session manager normally negotiates adapter ports. Configure
                # them explicitly here so the test never needs hardware policy.
                config_pod = '{ direction = "' + direction + '" mode = "dsp" format = { mediaType = "audio" mediaSubtype = "raw" format = "F32P" rate = 48000 channels = 2 position = [ FL FR ] } }'
                result = run(['pw-cli', 'set-param', str(node(name)), 'PortConfig', config_pod])
                assert 'Error:' not in result, result
            for channel in ('FL', 'FR'):
                run(['pw-link', f'BigSound.output:output_{channel}', f'qa-record:input_{channel}'])
                run(['pw-link', f'qa-source:output_{channel}', f'BigSound:playback_{channel}'])
            wait_for(lambda: '0.310000' in run(['pw-cli', 'enum-params', str(fresh_id), 'Props']), 'restored live tuning')
            print('D-Bus/PipeWire: 17 controls, 15 profiles, invalid input, persistence, 40 concurrent writes, failed-write/partial-profile rollback and filter-chain restart OK')

            cli('profile', 'apply', 'BigSound')
            time.sleep(1)
            source.terminate()
            source.wait(timeout=5)
            time.sleep(.2)
            recorder.terminate()
            recorder.wait(timeout=5)
            record_output.flush()
            output = array.array('f')
            output.frombytes(capture_file.read_bytes())
            assert len(output) > 48000 and all(math.isfinite(x) for x in output)
            peak = max(abs(x) for x in output)
            assert .01 < peak <= 1.000002, peak
            print(f'Real filter-chain audio: {len(output)//2} stereo frames captured, finite samples, peak {peak:.7f} <= 0 dBFS across changing profiles')

            if shutil.which('gtk4-broadwayd'):
                broadway = start(['gtk4-broadwayd', ':5'], 'broadway')
                time.sleep(.2)
                app_env = dict(env, GDK_BACKEND='broadway', BROADWAY_DISPLAY=':5')
                app_log = open(tmp / 'app.log', 'wb')
                files.append(app_log)
                app = subprocess.Popen([str(target / 'bigsound-app')], env=app_env, stdout=app_log, stderr=app_log)
                children.append(app)
                time.sleep(2)
                assert app.poll() is None, (tmp / 'app.log').read_text()
                app.terminate()
                app.wait(timeout=5)
                text = (tmp / 'app.log').read_text()
                assert 'CRITICAL' not in text and 'ERROR' not in text, text
                print('GTK/Broadway: application starts and remains alive without critical errors (smoke test)')
            else:
                print('GTK smoke test skipped: gtk4-broadwayd missing')
            assert daemon.poll() is None
        except Exception:
            if 'old_id' in locals() and old_id is not None:
                try:
                    print('Live Props at failure:\n' + run(['pw-cli', 'enum-params', str(old_id), 'Props']), file=sys.stderr)
                    if 'params' in locals():
                        print('Expected profile controls: ' + json.dumps(params, sort_keys=True), file=sys.stderr)
                except (AssertionError, subprocess.TimeoutExpired):
                    pass
            print(run(['pw-link', '-io']), file=sys.stderr)
            for log in tmp.glob('*.log'):
                text = log.read_text(errors='replace')
                print(log.name + ':\n' + text[-2000:], file=sys.stderr)
            raise
        finally:
            for proc in reversed(children):
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            for file in files:
                file.close()


if __name__ == '__main__':
    if '--private-bus' not in sys.argv:
        target = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release').resolve())
        with tempfile.TemporaryDirectory(prefix='bigsound-bus-') as bus_tmp:
            bus_config = Path(bus_tmp) / 'session.conf'
            # No service directories: never activate desktop portals, keyrings,
            # or other host services from this deliberately private bus.
            bus_config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=' + bus_tmp + '</listen>'
                                  '<policy context="default"><allow send_destination="*"/>'
                                  '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            raise SystemExit(subprocess.call(['dbus-run-session', '--config-file', str(bus_config), '--', sys.executable, str(Path(__file__).resolve()), target, '--private-bus']))
    main(Path(sys.argv[1]))
