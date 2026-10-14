# Tiny UDS client. usage: uds.py <sock> listen <secs> <out.jsonl> | uds.py <sock> send '<json>'
import socket, sys, json, time
sock, mode = sys.argv[1], sys.argv[2]
s = socket.socket(socket.AF_UNIX); s.connect(sock); s.settimeout(0.3)
if mode == "send":
    s.sendall((sys.argv[3] + "\n").encode()); time.sleep(0.4)
    buf = b""
    try:
        while True:
            d = s.recv(65536)
            if not d: break
            buf += d
    except Exception: pass
    for l in buf.decode(errors="replace").splitlines():
        if '"MasterPauseChanged"' in l or '"PauseSourceChanged"' in l: print("  " + l)
else:
    end = time.time() + float(sys.argv[3]); out = open(sys.argv[4], "w"); buf = b""
    print("smoke client connected", flush=True)
    while time.time() < end:
        try:
            d = s.recv(65536)
            if not d: break
            buf += d
            *lines, buf = buf.split(b"\n")
            for l in lines: out.write(json.dumps({"t": time.time(), "ev": l.decode(errors="replace")}) + "\n")
            out.flush()
        except socket.timeout: pass
