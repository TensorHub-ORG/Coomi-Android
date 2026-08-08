#!/bin/bash
set -e
sed -i 's|/opt/coomi-stats/venv/bin/uvicorn|/opt/coomi-stats/conda-env/bin/uvicorn|' /etc/systemd/system/coomi-stats.service
rm -rf /opt/coomi-stats/venv
systemctl daemon-reload
systemctl restart coomi-stats
sleep 5
echo "STATUS: $(systemctl is-active coomi-stats)"
echo "API: $(curl -s http://127.0.0.1:8062/api/stats)"
