#!/usr/bin/env python3
"""Capture real GTK UI in a private X11/D-Bus/PipeWire fixture."""
import argparse,json,os,hashlib,struct
from pathlib import Path
import shutil,subprocess,sys,tempfile,time

def wait_for(predicate, description):
    deadline = time.monotonic()+20
    while time.monotonic()<deadline:
        try:
            result = predicate()
            if result: return result
        except (OSError, ValueError, KeyError, AssertionError): pass
        time.sleep(.1)
    raise RuntimeError('Timed out: '+description)


def main(args):
    source = Path(args.source).resolve()
    target = Path(args.target).resolve()
    capture = Path(args.capture).resolve();capture.mkdir(parents=True,exist_ok=True)
    children=[];logs=[]
    with tempfile.TemporaryDirectory(prefix='bigsound-gui-') as temporary:
        work=Path(temporary)
        for d in ('runtime','config','data','cache'): (work/d).mkdir(mode=0o700)
        env=dict(os.environ,XDG_RUNTIME_DIR=str(work/'runtime'),PIPEWIRE_RUNTIME_DIR=str(work/'runtime'),XDG_CONFIG_HOME=str(work/'config'),XDG_DATA_HOME=str(work/'data'),XDG_CACHE_HOME=str(work/'cache'),
                 PULSE_SERVER='unix:'+str(work/'unavailable-pulse'),DBUS_SYSTEM_BUS_ADDRESS='unix:path='+str(work/'unavailable-system'),
                 LC_ALL='pt_BR.UTF-8',LANGUAGE='pt_BR',ADW_DEBUG_COLOR_SCHEME='prefer-'+args.theme,GSETTINGS_BACKEND='memory',GTK_A11Y='none',GTK_USE_PORTAL='0',GIO_USE_VFS='local')
        if args.display is None:
            args.display=next((n for n in range(119,319) if not Path('/tmp/.X11-unix/X'+str(n)).exists() and not Path('/tmp/.X'+str(n)+'-lock').exists()),None)
            if args.display is None:raise RuntimeError('No free private X11 display')
        if Path('/tmp/.X11-unix/X'+str(args.display)).exists() or Path('/tmp/.X'+str(args.display)+'-lock').exists():raise RuntimeError('The selected X11 display is already in use')
        display=':'+str(args.display)
        env.update(DISPLAY=display,GDK_BACKEND='x11',GDK_SCALE='2',GSK_RENDERER='cairo',LIBGL_ALWAYS_SOFTWARE='1')
        profiles=work/'config/bigsound/profiles';profiles.mkdir(parents=True)
        for f in (source/'crates/daemon/data/profiles').glob('*.json'):shutil.copyfile(f,profiles/f.name)
        dropin=work/'config/pipewire/filter-chain.conf.d';dropin.mkdir(parents=True)
        template=(source/'configs/pipewire/10-bigsound.conf.template').read_text()
        for name in ('bass','clarity','space','cross','loud'):template=template.replace('__BIG_'+name.upper()+'_SO_PATH__',str(target/f'libbig_{name}.so'))
        (dropin/'99-bigsound.conf').write_text(template)
        catalog=work/'data/locale/pt_BR/LC_MESSAGES/bigsound.mo';catalog.parent.mkdir(parents=True)
        subprocess.run(['msgfmt','--check','-o',str(catalog),str(source/'crates/gtk-app/po/pt_BR.po')],check=True)
        for kind,file in [('scalable','com.bigcommunity.BigSound.svg'),('symbolic','com.bigcommunity.BigSound-symbolic.svg')]:
            dest=work/'data/icons/hicolor'/kind/'apps'/file;dest.parent.mkdir(parents=True);shutil.copyfile(source/'crates/gtk-app/data/icons'/file,dest)
        theme=work/'data/icons/hicolor/index.theme'
        theme.write_text('[Icon Theme]\nName=BigSound test\nDirectories=scalable/apps,symbolic/apps\n[scalable/apps]\nSize=128\nType=Scalable\nMinSize=16\nMaxSize=512\nContext=Applications\n[symbolic/apps]\nSize=16\nType=Scalable\nMinSize=16\nMaxSize=512\nContext=Applications\n')

        def start(argv,name,extra=None):
            log=open(work/(name+'.log'),'wb');logs.append(log)
            proc=subprocess.Popen(argv,env=dict(env,**(extra or {})),stdout=log,stderr=log);children.append(proc);return proc
        def run(argv,check=True):
            result=subprocess.run(argv,env=env,text=True,capture_output=True,timeout=15)
            if check and result.returncode:raise RuntimeError(result.stderr[-1000:])
            return result.stdout.strip()
        def cli(*arguments):return run([str(target/'bigsound'),*arguments])
        def gapp(action):return run(['gapplication','action','com.bigcommunity.BigSound',action])
        try:
            xvfb=args.xvfb or shutil.which('Xvfb')
            if not xvfb:raise RuntimeError('Xvfb is required')
            authority=work/'Xauthority'
            env['XAUTHORITY']=str(authority)
            run(['xauth','-f',str(authority),'add',display,'.',os.urandom(16).hex()])
            authority.chmod(0o600)
            server=start([xvfb,display,'-auth',str(authority),'-nolisten','tcp','-noreset','-screen','0','2200x2400x24'],'xvfb')
            wait_for(lambda:server.poll() is None and subprocess.run(['xdotool','getdisplaygeometry'],env=env,capture_output=True).returncode==0,'private X11 display')
            if shutil.which('xsetroot'):run(['xsetroot','-solid','#fafafb'])
            start(['pipewire'],'pipewire')
            wait_for(lambda:(work/'runtime/pipewire-0').exists(),'private PipeWire')
            chain=start(['pipewire','-c','filter-chain.conf'],'chain')
            wait_for(lambda:'BigSound' in run(['pw-dump']),'LADSPA node')
            daemon=start([str(target/'bigsound-daemon')],'daemon')
            wait_for(lambda:subprocess.run([str(target/'bigsound'),'list'],env=env,capture_output=True).returncode==0,'audio daemon')
            cli('profile','apply','BigSound')
            app=start([str(target/'bigsound-app')],'app')
            def find_window():
                return run(['xdotool','search','--onlyvisible','--pid',str(app.pid),'--name','^BigSound$'],check=False).splitlines()
            window=wait_for(find_window,'GTK window')[0]
            time.sleep(2)
            geometry=run(['xdotool','getwindowgeometry','--shell',window])
            run(['xdotool','windowfocus','--sync',window])
            def themed(name):return Path(name).stem+'-'+args.theme+'.png'
            def shot(name):
                run(['xdotool','mousemove','2190','2390'])
                time.sleep(.2)
                run(['magick','import','-window',window,'-strip',str(capture/themed(name))])
                return themed(name)
            shot('main.png')
            gapp('advanced-view');time.sleep(.4);shot('advanced.png')
            gapp('main-view');cli('profile','apply','Voice / Podcast');time.sleep(3);shot('voice.png')
            cli('profile','apply','BigSound');time.sleep(3)
            print(json.dumps({'geometry':geometry,'captures':str(capture),'active':cli('profile','active')}),flush=True)
            captures=[]
            for name in ('main.png','advanced.png','voice.png'):
                data=(capture/themed(name)).read_bytes()
                assert data[:8]==b'\x89PNG\r\n\x1a\n'
                width,height=struct.unpack('>II',data[16:24])
                assert width>=1200 and height>=1200,(name,width,height)
                captures.append(hashlib.sha256(data).hexdigest())
            assert len(set(captures))==3,'Mode/profile views must differ'
            assert cli('profile','active')=='BigSound'
            assert app.poll() is None and daemon.poll() is None
            text=(work/'app.log').read_text(errors='replace')
            assert 'CRITICAL' not in text and 'ERROR' not in text,text[-1000:]
            print('GTK/X11 capture and mode/profile synchronization completed.',flush=True)
        except Exception:
            for name in ['app.log','xvfb.log']:
                file=work/name
                if file.exists():print(name+': '+file.read_text(errors='replace')[-2000:],flush=True)
            raise
        finally:
            for child in reversed(children):
                if child.poll() is None:
                    child.terminate()
                    try:child.wait(timeout=3)
                    except subprocess.TimeoutExpired:child.kill();child.wait()
            for log in logs:log.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    root=Path(__file__).resolve().parents[1]
    parser.add_argument('--source',default=str(root))
    parser.add_argument('--target',default=str(root/'target/release'))
    parser.add_argument('--capture',default=str(root/'validation-output/gui'))
    parser.add_argument('--theme',choices=('light','dark'),default='light')
    parser.add_argument('--xvfb')
    parser.add_argument('--display',type=int)
    parser.add_argument('--private-bus',action='store_true',help=argparse.SUPPRESS)
    args=parser.parse_args()
    if not args.private_bus:
        with tempfile.TemporaryDirectory(prefix='bigsound-gui-bus-') as directory:
            config=Path(directory)/'session.conf'
            config.write_text('<busconfig><type>session</type><listen>unix:tmpdir='+directory+'</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            raise SystemExit(subprocess.call(['dbus-run-session','--config-file',str(config),'--',sys.executable,str(Path(__file__).resolve()),*sys.argv[1:],'--private-bus']))
    main(args)
