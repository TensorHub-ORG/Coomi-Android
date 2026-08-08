import shutil
p = "/var/lib/pgsql/18.3/data/pg_hba.conf"
shutil.copy(p, p + ".bak")
lines = open(p).read().splitlines()
out = []
for ln in lines:
    if ln.strip().startswith("host") and "0.0.0.0/0" in ln:
        out.append("host    all             all             8.148.146.68/32            scram-sha-256")
        print("REPLACED:", ln.strip())
    else:
        out.append(ln)
open(p, "w").write("\n".join(out) + "\n")
print("DONE")
