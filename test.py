import subprocess
import time

DNS_SERVER = "127.0.0.2"

websites = open("websites.txt").readlines()
print("Creating threads for each website")
processes: list[subprocess.Popen] = [
    subprocess.Popen(["nslookup", website, DNS_SERVER], stdout=subprocess.PIPE)
    for website in websites
]

print("Waiting for threads to finish")
for p in processes:
    p.wait()
