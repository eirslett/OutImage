import { instantiateSimulaWasm } from "./wasm_host.mjs";

const canvas = document.getElementById("c");
const ctx = canvas.getContext("2d");
ctx.fillStyle = "#6cf";

const bytes = await fetch("./model.wasm").then((r) => {
  if (!r.ok) throw new Error("missing model.wasm — run ./run.sh in this directory");
  return r.arrayBuffer();
});
const { instance } = await instantiateSimulaWasm(bytes, {
  host: {
    plot(x, y) {
      const px = x * canvas.width;
      const py = (1 - y) * canvas.height;
      ctx.fillRect(px - 1, py - 1, 2, 2);
    },
  },
});

let t = 0;
function frame() {
  t += 0.012;
  instance.exports.tick(t);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
