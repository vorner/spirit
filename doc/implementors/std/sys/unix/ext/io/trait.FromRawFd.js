(function() {var implementors = {};
implementors["mio"] = [{"text":"impl FromRawFd for TcpStream","synthetic":false,"types":[]},{"text":"impl FromRawFd for TcpListener","synthetic":false,"types":[]},{"text":"impl FromRawFd for UdpSocket","synthetic":false,"types":[]}];
implementors["mio_uds"] = [{"text":"impl FromRawFd for UnixDatagram","synthetic":false,"types":[]},{"text":"impl FromRawFd for UnixListener","synthetic":false,"types":[]},{"text":"impl FromRawFd for UnixStream","synthetic":false,"types":[]}];
implementors["net2"] = [{"text":"impl FromRawFd for TcpBuilder","synthetic":false,"types":[]},{"text":"impl FromRawFd for UdpBuilder","synthetic":false,"types":[]}];
implementors["nix"] = [{"text":"impl FromRawFd for Inotify","synthetic":false,"types":[]},{"text":"impl FromRawFd for TimerFd","synthetic":false,"types":[]}];
implementors["socket2"] = [{"text":"impl FromRawFd for Socket","synthetic":false,"types":[]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()