import socket, struct, time
s = socket.create_connection(("129.204.62.207", 5432), timeout=8)
print("TCP-OK")
# SSLRequest: len=8, code=80877103
s.sendall(struct.pack(">II", 8, 80877103))
time.sleep(1.0)
try:
    data = s.recv(128)
    print("RESP:", data)
except socket.timeout:
    print("RESP: timeout")
s.close()
