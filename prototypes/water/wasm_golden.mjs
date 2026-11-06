// Runs every prototype scenario in the wasm32 build and prints
// "<scenario> <mode> <hash>" (then "<seam scenario> pair <hash>"), the same lines `water_proto --golden` prints natively.
import { readFileSync } from "node:fs";

const NAMES = ["settled", "small_spill", "big_drain", "bucket", "film", "u_tube", "dam_breach", "gate_flood", "pumped_cistern", "scoop"];
const MODES = ["cell", "bodies"];
const SEAMS = ["seam_drain", "seam_lake", "seam_bounce"];
const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[2]), {});
for (let s = 0; s < NAMES.length; s++) {
  for (let m = 0; m < MODES.length; m++) {
    const h = BigInt.asUintN(64, instance.exports.golden(s, m));
    console.log(`${NAMES[s]} ${MODES[m]} ${h.toString(16).padStart(16, "0")}`);
  }
}
for (let s = 0; s < SEAMS.length; s++) {
  const h = BigInt.asUintN(64, instance.exports.golden_seam(s));
  console.log(`${SEAMS[s]} pair ${h.toString(16).padStart(16, "0")}`);
}
