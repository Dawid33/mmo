// Runs every prototype scenario in the wasm32 build and prints
// "<scenario> <mode> <hash>", the same lines `water_proto --golden` prints natively.
import { readFileSync } from "node:fs";

const NAMES = ["settled", "small_spill", "big_drain", "bucket", "film", "u_tube"];
const MODES = ["cell", "bodies"];
const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[2]), {});
for (let s = 0; s < NAMES.length; s++) {
  for (let m = 0; m < MODES.length; m++) {
    const h = BigInt.asUintN(64, instance.exports.golden(s, m));
    console.log(`${NAMES[s]} ${MODES[m]} ${h.toString(16).padStart(16, "0")}`);
  }
}
