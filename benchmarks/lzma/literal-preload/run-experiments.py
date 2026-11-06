import subprocess, re, json, sys, os, time
S=os.path.dirname(os.path.abspath(__file__))
shas=["95e69b5","138e57b","c6e1541","29cb44e"]
out=[]
def busy():
    ps=subprocess.run(["ps","-Ao","pcpu,comm"],capture_output=True,text=True).stdout.splitlines()[1:]
    return [l.strip() for l in ps if float(l.split()[0])>=25 and "probe-" not in l]
for rnd in range(9):
    order=shas[rnd%4:]+shas[:rnd%4]
    for sha in order:
        b=busy()
        r=subprocess.run([f"{S}/probe-{sha}","lzma::speed::decode_speed_against_liblzma","--exact","--ignored","--nocapture"],cwd=S,capture_output=True,text=True)
        for m in re.finditer(r"round (\d+): zipper ([\d.]+)(m?s), liblzma ([\d.]+)(m?s)",r.stdout+r.stderr):
            out.append(dict(warmup=rnd==0,round=rnd,sha=sha,inner=int(m[1]),zipper=float(m[2])*(1 if m[3]=="ms" else 1000),liblzma=float(m[4])*(1 if m[5]=="ms" else 1000),busy=b))
        print(rnd,sha,[o["zipper"] for o in out if o["round"]==rnd and o["sha"]==sha],b,flush=True)
json.dump(out,open(f"{S}/samples-experiments.json","w"),indent=1)
