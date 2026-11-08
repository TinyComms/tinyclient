import json
from pathlib import Path
import socket
import struct
import sys
import tempfile
import threading
import unittest

import ipc


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux Unix-socket transport")
class UnixIpcTests(unittest.TestCase):
    def test_request_uses_the_profile_socket_and_length_prefixed_json(self):
        with tempfile.TemporaryDirectory() as folder:
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(str(Path(folder) / "chat.sock"))
            server.listen(1)
            server.settimeout(1)
            seen = []
            response = {"op": "status", "accounts": []}

            def serve():
                try:
                    connection, _ = server.accept()
                    with connection:
                        connection.settimeout(2)
                        def read_exact(length):
                            data = b""
                            while len(data) < length:
                                chunk = connection.recv(length - len(data))
                                if not chunk:
                                    raise OSError("fixture client closed a partial frame")
                                data += chunk
                            return data
                        size = struct.unpack("!I", read_exact(4))[0]
                        data = read_exact(size)
                        seen.append(json.loads(data))
                        encoded = json.dumps(response).encode()
                        connection.sendall(struct.pack("!I", len(encoded)) + encoded)
                except (OSError, ValueError):
                    pass

            peer = threading.Thread(target=serve)
            peer.start()
            try:
                try:
                    answer = ipc.request(folder, {"op": "status"}, app_dir=folder, timeout=2)
                except RuntimeError as error:
                    answer = {"op": "error", "detail": str(error)}
                self.assertEqual(answer, response)
                self.assertEqual(seen, [{"op": "status"}])
            finally:
                server.close()
                peer.join(timeout=3)
                self.assertFalse(peer.is_alive())


if __name__ == "__main__":
    unittest.main()
