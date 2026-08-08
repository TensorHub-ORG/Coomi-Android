set -e
/root/miniconda3/bin/conda create -p /opt/coomi-stats/conda-env -y python=3.13 -c https://mirrors.tuna.tsinghua.edu.cn/anaconda/pkgs/main 2>&1 | tail -3
/opt/coomi-stats/conda-env/bin/pip install -q -i https://pypi.tuna.tsinghua.edu.cn/simple fastapi "uvicorn[standard]" psycopg2-binary 2>&1 | tail -2
chown -R www:www /opt/coomi-stats/conda-env
echo "===VERIFY==="
runuser -u www -- /opt/coomi-stats/conda-env/bin/uvicorn --version 2>&1 | head -1
