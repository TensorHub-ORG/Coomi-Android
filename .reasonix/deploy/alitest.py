import os
import psycopg
os.environ["PGPASSWORD"] = "j9P5avVg0BP_uOnohvoFslb0GJRLkaJBXvalNW7AgWtpQN5T"
c = psycopg.connect(host="129.204.62.207", port=5432, user="coomi_stats", dbname="coomi_stats", connect_timeout=8)
print("ALI-DIRECT-OK", c.execute("SELECT count(*) FROM counters").fetchone())
c.close()
