/* Host files and nonblocking console input for the Node launcher. */
if (ENVIRONMENT_IS_NODE && !ENVIRONMENT_IS_PTHREAD) {
    const onExit = Module.onExit;
    Module.onExit = function (status) {
        process.stdin.pause();
        onExit?.(status);
    };
    Module.onRuntimeInitialized = function () {
        FS.mkdir('/host');
        FS.mount(NODEFS, {root: '/'}, '/host');
        const input = [];
        let ended = false;
        const stdin = FS.getStream(0);
        process.stdin.on('data', data => {
            input.push(...data);
            stdin.node.notifyListeners?.(1); // POLLIN: wake a pending poll().
        });
        process.stdin.on('end', () => {
            ended = true;
            stdin.node.notifyListeners?.(17); // POLLIN | POLLHUP.
        });
        TTY.default_tty_ops.get_char = () => input.length ? input.shift() :
            (ended ? null : undefined);
        stdin.stream_ops = {...stdin.stream_ops,
            poll: () => (input.length || ended ? 1 : 0)};
    };
}
