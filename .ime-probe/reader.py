import sys, pathlib, time
p = pathlib.Path(r"F:\projects\sainput\.ime-probe\received.txt")
line = sys.stdin.readline()
p.write_text(repr(line), encoding="utf-8")
sys.stdout.write("READER GOT " + repr(line) + "\n")
sys.stdout.flush()
time.sleep(120)
