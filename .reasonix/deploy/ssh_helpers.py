"""SSH/SFTP 部署辅助脚本（Coomi-Android 发行部署用）。

用法:
  python ssh_helpers.py run <host> <port> <user> <auth> <command>
     auth = password:<明文密码> 或 key:<密钥路径>
     command = 远程要执行的 shell 命令
  python ssh_helpers.py put <host> <port> <user> <auth> <local> <remote>
     auth = password:<明文密码> 或 key:<密钥路径>
"""
import sys
import paramiko


def _connect(host, port, user, auth):
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    if auth.startswith("password:"):
        client.connect(host, port=port, username=user, password=auth[len("password:"):], timeout=20)
    elif auth.startswith("key:"):
        client.connect(host, port=port, username=user, key_filename=auth[len("key:"):], timeout=20)
    else:
        raise ValueError("auth 必须是 password:<pw> 或 key:<path>")
    return client


def run(host, port, user, auth, command):
    client = _connect(host, port, user, auth)
    stdin, stdout, stderr = client.exec_command(command, timeout=120)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    print(out, end="")
    if err:
        print("[stderr]", err, end="", file=sys.stderr)
    client.close()
    sys.exit(code)


def put(host, port, user, auth, local, remote):
    client = _connect(host, port, user, auth)
    sftp = client.open_sftp()
    sftp.put(local, remote)
    sftp.close()
    client.close()
    print(f"uploaded {local} -> {remote}")


def get(host, port, user, auth, remote, local):
    client = _connect(host, port, user, auth)
    sftp = client.open_sftp()
    sftp.get(remote, local)
    sftp.close()
    client.close()
    print(f"downloaded {remote} -> {local}")


if __name__ == "__main__":
    cmd = sys.argv[1]
    host, port, user, auth = sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5]
    if cmd == "run":
        run(host, port, user, auth, sys.argv[6])
    elif cmd == "put":
        put(host, port, user, auth, sys.argv[6], sys.argv[7])
    elif cmd == "get":
        get(host, port, user, auth, sys.argv[6], sys.argv[7])
    else:
        raise SystemExit("unknown command: " + cmd)
