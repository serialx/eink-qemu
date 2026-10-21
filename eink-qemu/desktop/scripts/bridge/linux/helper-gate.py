"""Disposable-VM gate for the installed managed helper; never changes the uplink."""
import shutil, array, fcntl, json, os, pathlib, pwd, socket, stat, subprocess, sys
helper = pathlib.Path('/usr/lib/eink-qemu/bridge/eink-qemu-bridge-helper')
acl = pathlib.Path('/etc/eink-qemu/bridge/bridge.conf')
bridge = 'einkgate0'

def run(*args):
    return subprocess.check_output(args, text=True).strip()

def open_as_user():
    assert os.getuid() != 0 and os.geteuid() != 0
    received = []
    for interface, allowed in [(bridge, '--deny' not in sys.argv), ('not-authorized', False)]:
        parent, child = socket.socketpair()
        parent.settimeout(5)
        proc = subprocess.Popen([str(helper), '--br='+interface, '--fd='+str(child.fileno())], pass_fds=[child.fileno()], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        child.close()
        message, ancillary, _, _ = parent.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
        out, err = proc.communicate(timeout=5)
        fds = array.array('i')
        for level, kind, payload in ancillary:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                fds.frombytes(payload[:len(payload) - len(payload) % fds.itemsize])
        if allowed:
            assert proc.returncode == 0 and len(fds) == 1, (proc.returncode, err)
            fd = fds[0]
            try:
                name = fcntl.ioctl(fd, 0x800454d2, bytes(40))[:16].split(b'\0')[0].decode()
                assert pathlib.Path('/sys/class/net', name, 'master').resolve().name == bridge
                assert os.write(fd, bytes.fromhex('ffffffffffff0200000000010800') + bytes(46)) == 60
                received.append({'uid':os.getuid(), 'euid':os.geteuid(), 'tap':name, 'memberOf':bridge, 'packetWrite':'passed'})
            finally: os.close(fd)
        else:
            assert proc.returncode != 0 and not fds and b'access denied' in err, (proc.returncode, err)
        parent.close()
    print(json.dumps({'openAsUser':'passed','unauthorizedBridge':'denied','transport':received}))

if '--user' in sys.argv:
    open_as_user()
else:
    assert os.getuid() == 0 and socket.gethostname() == 'eink-bridge-test'
    assert not acl.exists() and not pathlib.Path('/sys/class/net',bridge).exists()
    assert not helper.exists()
    helper.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile('/var/tmp/eink-helper-source/build/eink-qemu-bridge-helper', helper)
    oldmode = 0o755
    before = json.loads(run('ip','-j','address','show','dev','enp0s1'))
    try:
        acl.parent.mkdir(parents=True, exist_ok=True)
        helper.chmod(0o4755)
        if '--policy-gate' in sys.argv:
            subprocess.run(['/home/ubuntu/eink-bridge-tests/target/debug/examples/linux-policy-gate'], check=True)
        acl.write_text('allow 1000 '+bridge+'\n')
        acl.chmod(0o644)
        helper.chmod(0o4755)
        run('ip','link','add',bridge,'type','bridge')
        run('ip','link','set',bridge,'up')
        if '--policy-gate' in sys.argv:
            subprocess.run(['/home/ubuntu/eink-bridge-tests/target/debug/examples/linux-policy-gate', '--probe'], check=True)
        account = pwd.getpwnam('ubuntu')
        def drop():
            os.setgroups([]); os.setgid(account.pw_gid); os.setuid(account.pw_uid)
        result = subprocess.run([sys.executable,__file__,'--user'], preexec_fn=drop, capture_output=True, text=True, timeout=15)
        print(result.stdout, end='')
        assert result.returncode == 0, result.stderr
        # The same installed helper and bridge must reject the wrong principal,
        # unsafe policy permissions, and symlink substitution.
        for policy in ('wrong-user', 'writable', 'symlink'):
            if policy == 'wrong-user':
                acl.write_text('allow 1001 '+bridge+'\n')
            elif policy == 'writable':
                acl.write_text('allow 1000 '+bridge+'\n'); acl.chmod(0o666)
            else:
                acl.unlink(); acl.symlink_to('/dev/null')
            denied = subprocess.run([sys.executable,__file__,'--user','--deny'], preexec_fn=drop, capture_output=True, text=True, timeout=15)
            assert denied.returncode == 0, (policy, denied.stderr)
            print(json.dumps({'policy':policy,'denied':True}))
    finally:
        helper.chmod(oldmode)
        helper.unlink()
        acl.unlink(missing_ok=True)
        subprocess.run(['ip','link','del',bridge], check=False)
    after = json.loads(run('ip','-j','address','show','dev','enp0s1'))
    def addresses(state): return [(a['local'],a['prefixlen']) for i in state for a in i['addr_info']]
    assert addresses(before) == addresses(after)
    print(json.dumps({'restore':'passed','source':'dedicated app helper built from this worktree with fixed ACL path'}))
