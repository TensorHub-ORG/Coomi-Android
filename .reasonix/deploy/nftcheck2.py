import subprocess
for chain in ["filter_IN_public", "filter_IN_public_allow", "filter_IN_public_deny"]:
    out = subprocess.run(["nft", "list", "chain", "inet", "firewalld", chain],
                         capture_output=True, text=True)
    print("====", chain, "rc=", out.returncode)
    if out.stdout.strip():
        for ln in out.stdout.splitlines():
            print("  ", ln.strip()[:200])
