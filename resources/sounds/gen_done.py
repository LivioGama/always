# Three softer/shorter "done" variants in the halo family, via gen.py's own renderer.
import sys, os, importlib.util
G = sys.argv[1]; out = sys.argv[2]
spec = importlib.util.spec_from_file_location("gen", os.path.join(G, "gen.py")); gen = importlib.util.module_from_spec(spec)
sys.argv = ["gen.py", "/nonexistent-no-build"]
src = open(os.path.join(G, "gen.py")).read().replace('if __name__ == "__main__":', 'if False:')
exec(compile(src, "gen.py", "exec"), gen.__dict__)
N = gen.N; halo = gen.FAMILIES["halo"]
gen.TARGET_MAX_M = -33.0   # 3 dB softer than the -30 LUFS set
variants = {
    # A: rising major third F#5 -> A5, shorter tau, no octave halo
    "done_a": ([N("F#5", 260, 0, 0.6, tau=55), N("A5", 340, 40, 0.9, tau=60)], 420),
    # B: single soft A5 with a faint D6 shimmer (bell), short
    "done_b": ([N("A5", 320, 0, 0.9, tau=58), N("D6", 320, 0, 0.12, tick_db=-90, tau=50)], 380),
    # C: the original arpeggio, compressed (25 ms steps), no D6, shorter tau
    "done_c": ([N("D5", 240, 0, 0.4, tau=50), N("F#5", 240, 25, 0.5, tau=50), N("A5", 340, 50, 0.85, tau=58)], 440),
}
gen.FAMILIES = {"halodone": dict(halo, events=variants)}
gen.build(out)
