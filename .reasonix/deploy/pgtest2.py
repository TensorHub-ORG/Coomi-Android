import socket, struct, time
s = socket.create_connection(("129.204.62.207", 5432), timeout=8)
print("TCP-OK")
def msg(body: bytes) -> bytes:
    return struct.pack(">I", len(body) + 4) + body
params = b"\x00user\x00coomi_stats\x00database\x00coomi_stats\x00\x00"
startup = struct.pack(">I", 4 + 4 + len(params)) + struct.pack(">I", 196608) + params
s.sendall(startup)
s.settimeout(6)
try:
    total = b""
    while True:
        chunk = s.recv(4096)
        if not chunk:
            break
        total += chunk
    print("PG-RESP:", total[:200])
except socket.timeout:
    print("PG-RESP: timeout")
finally:
    s.close()
