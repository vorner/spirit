(function() {var implementors = {};
implementors["mio"] = [{"text":"impl IntoRawFd for TcpStream","synthetic":false,"types":[]},{"text":"impl IntoRawFd for TcpListener","synthetic":false,"types":[]},{"text":"impl IntoRawFd for UdpSocket","synthetic":false,"types":[]}];
implementors["mio_uds"] = [{"text":"impl IntoRawFd for UnixDatagram","synthetic":false,"types":[]},{"text":"impl IntoRawFd for UnixListener","synthetic":false,"types":[]},{"text":"impl IntoRawFd for UnixStream","synthetic":false,"types":[]}];
implementors["nix"] = [{"text":"impl IntoRawFd for PtyMaster","synthetic":false,"types":[]}];
implementors["socket2"] = [{"text":"impl IntoRawFd for Socket","synthetic":false,"types":[]}];
if (window.register_implementors) {window.register_implementors(implementors);} else {window.pending_implementors = implementors;}})()