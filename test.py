import subprocess
import time

# DNS_SERVER = "8.8.8.8"
DNS_SERVER = "127.0.0.2"
# DNS_SERVER = "192.168.1.1"
DELAY = 0.003

websites = filter(lambda x: bool(x.strip()), open("websites.txt").readlines())
print("Creating threads for each website")
processes: list[subprocess.Popen] = [
    subprocess.Popen(["nslookup", website, DNS_SERVER], stdout=subprocess.PIPE)
    for website in websites if time.sleep(DELAY) is None
]

print("Waiting for threads to finish")
for p in processes:
    p.wait()
