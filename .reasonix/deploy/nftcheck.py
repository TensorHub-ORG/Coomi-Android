import subprocess
out = subprocess.run(["nft", "list", "chain", "inet", "firewalld", "filter_IN_public"],
                     capture_output=True, text=True)
print("RC:", out.returncode)
for ln in out.stdout.splitlines():
    if "5432" in ln or "reject" in ln or "drop" in ln:
        print(ln.strip()[:160])
print("---DONE---")
