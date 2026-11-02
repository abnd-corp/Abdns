"""Banc de mesure d'abdnsd (`make bench`) : taille, mémoire, fils, processeur, débit.

Lance le service compilé en `--no-system` sur un port libre, avec deux serveurs DoH publics :
il faut une connexion à Internet. Les résultats dépendent du poste et du réseau ; on compare
avant et après un changement sur la même machine, pas d'une machine à l'autre.
"""

import os, platform, socket, struct, subprocess, sys, tempfile, threading, time

PORT = 5399
CONFIG = """language = "en"
listen = ["127.0.0.1:{port}"]
state_dir = "{dir}/state"
clear_log = "{dir}/clear.log"
local_fallback = false
[[server]]
name = "Cloudflare"
url = "https://1.1.1.1/dns-query"
[[server]]
name = "Google"
url = "https://8.8.8.8/dns-query"
"""


def query(name, ident):
    header = struct.pack(">HHHHHH", ident & 0xFFFF, 0x0100, 1, 0, 0, 0)
    labels = b"".join(bytes([len(p)]) + p.encode() for p in name.split("."))
    return header + labels + b"\0" + struct.pack(">HH", 1, 1)


def ask_all(names, out):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(5)
    answered = 0
    for ident, name in enumerate(names):
        sock.sendto(query(name, ident), ("127.0.0.1", PORT))
        try:
            sock.recv(4096)
            answered += 1
        except socket.timeout:
            pass
    out.append(answered)


def parallel(groups):
    out, started = [], time.monotonic()
    threads = [threading.Thread(target=ask_all, args=(g, out)) for g in groups]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return sum(out), time.monotonic() - started


def cpu_seconds(pid):
    text = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    minutes, seconds = text.split(":")[-2:]
    hours = text.split(":")[0] if text.count(":") == 2 else "0"
    return int(hours) * 3600 + int(minutes) * 60 + float(seconds)


def rss_mib(pid):
    return int(subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout) / 1024


def threads(pid):
    if platform.system() == "Linux":
        return len(os.listdir(f"/proc/{pid}/task"))
    return len(subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/release/abdnsd"
    tag = str(os.getpid())
    rows = [("Binary", f"{os.path.getsize(binary) / 1048576:.2f} MiB")]
    with tempfile.TemporaryDirectory(prefix="abdns-bench-", dir="/tmp") as work:
        path = os.path.join(work, "config.toml")
        with open(path, "w") as f:
            f.write(CONFIG.format(port=PORT, dir=work))
        service = subprocess.Popen([binary, "--config", path, "--no-system"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(6)
            pid = service.pid
            rows += [("Idle memory (RSS)", f"{rss_mib(pid):.1f} MiB"), ("Idle threads", str(threads(pid)))]

            warm = ["example.com", "wikipedia.org", "github.com", "apple.com"]
            parallel([warm])
            before = cpu_seconds(pid)
            count, secs = parallel([warm * 5000 for _ in range(8)])
            spent = cpu_seconds(pid) - before
            rows += [("Cached: answers/s (8 clients)", f"{count / secs:,.0f}"), ("Cached: CPU per answer", f"{spent / count * 1e6:.0f} µs")]

            fresh = [f"n{i}.{tag}.abdns-bench.example.org" for i in range(1500)]
            before = cpu_seconds(pid)
            count, secs = parallel([fresh[k::16] for k in range(16)])
            spent = cpu_seconds(pid) - before
            rows += [("New names: 1500 in (16 clients)", f"{secs:.1f} s"), ("New names: CPU per query", f"{spent / count * 1e3:.2f} ms")]

            burst = [[f"b{i}.{tag}.abdns-bench.example.org"] for i in range(30)]
            _, secs = parallel(burst)
            rows += [("Burst: 30 new names at once", f"{secs * 1000:.0f} ms"), ("Memory after load (RSS)", f"{rss_mib(pid):.1f} MiB"), ("Threads after load", str(threads(pid)))]
        finally:
            service.terminate()
            service.wait()
    width = max(len(label) for label, _ in rows)
    for label, value in rows:
        print(f"{label:<{width}}  {value}")


if __name__ == "__main__":
    main()
