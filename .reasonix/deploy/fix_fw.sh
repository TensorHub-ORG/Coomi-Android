#!/bin/bash
set -e
firewall-cmd --permanent --remove-port=5432/tcp || true
firewall-cmd --permanent --add-rich-rule='rule family="ipv4" source address="8.148.146.68" port port="5432" protocol="tcp" accept'
firewall-cmd --reload
echo '===CHECK==='
firewall-cmd --list-all | grep -E '5432|ports|rich'
